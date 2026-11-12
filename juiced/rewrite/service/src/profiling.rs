#[cfg(all(unix, feature = "cpu-profile"))]
pub struct CpuProfile {
    guard: pprof::ProfilerGuard<'static>,
    output: std::path::PathBuf,
}

#[cfg(all(unix, feature = "cpu-profile"))]
impl CpuProfile {
    pub fn from_env() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Some(path) = std::env::var_os("JUICED_CPU_PROFILE") else {
            return Ok(None);
        };
        let guard = pprof::ProfilerGuardBuilder::default()
            .frequency(199)
            .blocklist(&["libc", "libgcc", "pthread", "vdso"])
            .build()?;
        Ok(Some(Self {
            guard,
            output: path.into(),
        }))
    }

    pub fn finish(self) -> Result<(), Box<dyn std::error::Error>> {
        let report = self.guard.report().build()?;
        let output = std::fs::File::create(self.output)?;
        report.flamegraph(output)?;
        Ok(())
    }
}
