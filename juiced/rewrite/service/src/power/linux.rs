use super::{Backend, NativeLease};
use std::{
    ffi::{c_char, c_int, c_void},
    os::fd::{FromRawFd, OwnedFd},
    ptr,
    sync::OnceLock,
    time::Instant,
};

pub(super) struct Native {
    api: OnceLock<Api>,
}

impl Native {
    pub(super) fn new() -> Self {
        Self {
            api: OnceLock::new(),
        }
    }
}

impl Backend for Native {
    fn platform(&self) -> &'static str {
        "linux-logind"
    }
    fn acquire(&self, deadline: Instant) -> Result<NativeLease, &'static str> {
        if Instant::now() >= deadline {
            return Err("inhibitor_acquisition_deadline");
        }
        if self.api.get().is_none() {
            let _ = self.api.set(Api::load()?);
        }
        let api = self.api.get().ok_or("libsystemd_abi_unavailable")?;
        let mut exchange = Exchange {
            api,
            bus: ptr::null_mut(),
            request: ptr::null_mut(),
            reply: ptr::null_mut(),
            error: BusError {
                name: ptr::null(),
                message: ptr::null(),
                need_free: 0,
            },
        };
        if unsafe { (api.open_system)(&mut exchange.bus) } < 0 || exchange.bus.is_null() {
            return Err("logind_bus_unavailable");
        }
        if Instant::now() >= deadline {
            return Err("inhibitor_acquisition_deadline");
        }
        if unsafe {
            (api.new_method)(
                exchange.bus,
                &mut exchange.request,
                c"org.freedesktop.login1".as_ptr(),
                c"/org/freedesktop/login1".as_ptr(),
                c"org.freedesktop.login1.Manager".as_ptr(),
                c"Inhibit".as_ptr(),
            )
        } < 0
            || exchange.request.is_null()
        {
            return Err("logind_request_unavailable");
        }
        if unsafe {
            (api.append)(
                exchange.request,
                c"ssss".as_ptr(),
                c"sleep:idle".as_ptr(),
                c"Juiced".as_ptr(),
                c"Model worker activity".as_ptr(),
                c"block".as_ptr(),
            )
        } < 0
        {
            return Err("logind_request_invalid");
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_micros();
        if remaining == 0 {
            return Err("inhibitor_acquisition_deadline");
        }
        if unsafe {
            (api.call)(
                exchange.bus,
                exchange.request,
                remaining.min(u64::MAX as u128) as u64,
                &mut exchange.error,
                &mut exchange.reply,
            )
        } < 0
            || exchange.reply.is_null()
        {
            return Err("logind_inhibit_unavailable");
        }
        let mut borrowed = -1;
        if unsafe { (api.read)(exchange.reply, c"h".as_ptr(), &mut borrowed as *mut c_int) } <= 0
            || borrowed < 0
        {
            return Err("logind_reply_invalid");
        }
        // sd_bus_message_read borrows its FD; retain a close-on-exec duplicate before unref.
        let fd = unsafe { libc::fcntl(borrowed, libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err("logind_descriptor_unavailable");
        }
        Ok(Box::new(unsafe { OwnedFd::from_raw_fd(fd) }))
    }
}

#[repr(C)]
struct BusError {
    name: *const c_char,
    message: *const c_char,
    need_free: c_int,
}

type OpenSystem = unsafe extern "C" fn(*mut *mut c_void) -> c_int;
type NewMethod = unsafe extern "C" fn(
    *mut c_void,
    *mut *mut c_void,
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
) -> c_int;
type MessageVarargs = unsafe extern "C" fn(*mut c_void, *const c_char, ...) -> c_int;
type Call =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u64, *mut BusError, *mut *mut c_void) -> c_int;
type Unref = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type FreeError = unsafe extern "C" fn(*mut BusError);

struct Library(*mut c_void);

// Only immutable symbol addresses cross threads; each bus/message stays on its native worker.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Drop for Library {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                libc::dlclose(self.0);
            }
        }
    }
}

struct Api {
    _library: Library,
    open_system: OpenSystem,
    new_method: NewMethod,
    append: MessageVarargs,
    read: MessageVarargs,
    call: Call,
    unref_message: Unref,
    unref_bus: Unref,
    free_error: FreeError,
}

impl Api {
    fn load() -> Result<Self, &'static str> {
        let library = Library(unsafe {
            libc::dlopen(
                c"libsystemd.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        });
        if library.0.is_null() {
            return Err("libsystemd_unavailable");
        }
        macro_rules! symbol {
            ($name:literal, $kind:ty) => {{
                let address = unsafe { libc::dlsym(library.0, $name.as_ptr()) };
                if address.is_null() {
                    return Err("libsystemd_abi_unavailable");
                }
                unsafe { std::mem::transmute::<*mut c_void, $kind>(address) }
            }};
        }
        Ok(Self {
            open_system: symbol!(c"sd_bus_open_system", OpenSystem),
            new_method: symbol!(c"sd_bus_message_new_method_call", NewMethod),
            append: symbol!(c"sd_bus_message_append", MessageVarargs),
            read: symbol!(c"sd_bus_message_read", MessageVarargs),
            call: symbol!(c"sd_bus_call", Call),
            unref_message: symbol!(c"sd_bus_message_unref", Unref),
            unref_bus: symbol!(c"sd_bus_unref", Unref),
            free_error: symbol!(c"sd_bus_error_free", FreeError),
            _library: library,
        })
    }
}

struct Exchange<'a> {
    api: &'a Api,
    bus: *mut c_void,
    request: *mut c_void,
    reply: *mut c_void,
    error: BusError,
}

impl Drop for Exchange<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.api.unref_message)(self.reply);
            (self.api.unref_message)(self.request);
            (self.api.free_error)(&mut self.error);
            (self.api.unref_bus)(self.bus);
        }
    }
}
