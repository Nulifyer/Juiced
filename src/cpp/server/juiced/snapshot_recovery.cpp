#include "lemon/juiced/snapshot_recovery.h"
#include "lemon/utils/content_hash.h"

#include <algorithm>
#include <array>
#include <cerrno>
#include <istream>
#include <map>
#include <memory>
#include <set>
#include <stdexcept>
#include <streambuf>
#include <vector>

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#else
#include <dirent.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace lemon {
namespace {
namespace fs = std::filesystem;
using json = nlohmann::json;
constexpr uint64_t maximum_state_bytes = 16 * 1024 * 1024;
constexpr size_t maximum_json_events = 262144;
constexpr size_t maximum_json_depth = 64;

bool natural(const json& value) {
    return value.is_number_unsigned() || (value.is_number_integer() && value.get<int64_t>() >= 0);
}
bool text(const json& value, size_t maximum = 160) {
    return value.is_string() && !value.get_ref<const std::string&>().empty() &&
        value.get_ref<const std::string&>().size() <= maximum &&
        value.get_ref<const std::string&>().find('\0') == std::string::npos;
}
bool checksum_metadata(const json& entry) {
    return entry.is_object() && entry.contains("bytes") && natural(entry["bytes"]) &&
        entry.contains("checksum") && entry["checksum"].is_string() &&
        snapshot_metadata::valid_id(entry["checksum"].get_ref<const std::string&>());
}
struct Stopped {};
struct InvalidStructure {};

struct FileStamp {
    uint64_t bytes = 0, device = 0, inode = 0;
    std::array<unsigned char, 16> file_id{};
    uint64_t modified = 0, modified_fraction = 0, changed = 0, changed_fraction = 0;
    bool same_identity(const FileStamp& other) const {
        return device == other.device && inode == other.inode && file_id == other.file_id;
    }
    bool operator==(const FileStamp& other) const {
        return same_identity(other) && bytes == other.bytes && modified == other.modified &&
            modified_fraction == other.modified_fraction && changed == other.changed && changed_fraction == other.changed_fraction;
    }
};
enum class FileState { absent, invalid, present };

#ifdef _WIN32
using InputHandle = HANDLE;
const InputHandle invalid_input = INVALID_HANDLE_VALUE;
void close_input(InputHandle value) { if (value != invalid_input) CloseHandle(value); }
bool input_stamp(InputHandle input, bool directory, FileStamp& result) {
    BY_HANDLE_FILE_INFORMATION info{};
    FILE_BASIC_INFO basic{};
    FILE_ID_INFO identity{};
    if (GetFileType(input) != FILE_TYPE_DISK || !GetFileInformationByHandle(input, &info) ||
        !GetFileInformationByHandleEx(input, FileBasicInfo, &basic, sizeof(basic)) ||
        !GetFileInformationByHandleEx(input, FileIdInfo, &identity, sizeof(identity)) ||
        (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) ||
        bool(info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != directory || (!directory && info.nNumberOfLinks != 1)) return false;
    result.bytes = (uint64_t(info.nFileSizeHigh) << 32) | info.nFileSizeLow;
    result.device = identity.VolumeSerialNumber;
    result.inode = 0;
    std::copy_n(identity.FileId.Identifier, result.file_id.size(), result.file_id.begin());
    result.modified = (uint64_t(info.ftLastWriteTime.dwHighDateTime) << 32) | info.ftLastWriteTime.dwLowDateTime;
    result.changed = static_cast<uint64_t>(basic.ChangeTime.QuadPart);
    return true;
}
#else
using InputHandle = int;
constexpr InputHandle invalid_input = -1;
void close_input(InputHandle value) { if (value != invalid_input) ::close(value); }
bool stat_stamp(const struct stat& info, bool directory, FileStamp& result) {
    if ((directory ? !S_ISDIR(info.st_mode) : (!S_ISREG(info.st_mode) || info.st_nlink != 1)) || info.st_size < 0) return false;
    result.bytes = static_cast<uint64_t>(info.st_size);
    result.device = static_cast<uint64_t>(info.st_dev);
    result.inode = static_cast<uint64_t>(info.st_ino);
#ifdef __APPLE__
    result.modified = static_cast<uint64_t>(info.st_mtimespec.tv_sec);
    result.modified_fraction = static_cast<uint64_t>(info.st_mtimespec.tv_nsec);
    result.changed = static_cast<uint64_t>(info.st_ctimespec.tv_sec);
    result.changed_fraction = static_cast<uint64_t>(info.st_ctimespec.tv_nsec);
#else
    result.modified = static_cast<uint64_t>(info.st_mtim.tv_sec);
    result.modified_fraction = static_cast<uint64_t>(info.st_mtim.tv_nsec);
    result.changed = static_cast<uint64_t>(info.st_ctim.tv_sec);
    result.changed_fraction = static_cast<uint64_t>(info.st_ctim.tv_nsec);
#endif
    return true;
}
bool input_stamp(InputHandle input, bool directory, FileStamp& result) {
    struct stat info{};
    return ::fstat(input, &info) == 0 && stat_stamp(info, directory, result);
}
#endif

struct RetainedInput {
    InputHandle value = invalid_input;
    RetainedInput() = default;
    explicit RetainedInput(InputHandle input) : value(input) {}
    RetainedInput(const RetainedInput&) = delete;
    RetainedInput& operator=(const RetainedInput&) = delete;
    RetainedInput(RetainedInput&& other) noexcept : value(other.value) { other.value = invalid_input; }
    ~RetainedInput() { close_input(value); }
};

class InspectionInput {
public:
    InspectionInput(InputHandle input, const FileStamp& stamp) : handle_(input), before_(stamp) {}
    const FileStamp& before() const { return before_; }
    bool unchanged() const {
        FileStamp after;
        return input_stamp(handle_.value, false, after) && after == before_;
    }
    std::unique_ptr<std::streambuf> rewind() {
#ifdef _WIN32
        LARGE_INTEGER zero{};
        if (!SetFilePointerEx(handle_.value, zero, nullptr, FILE_BEGIN)) return nullptr;
#else
        if (::lseek(handle_.value, 0, SEEK_SET) != 0) return nullptr;
#endif
        return std::make_unique<ReadBuffer>(handle_.value, before_.bytes);
    }

private:
    class ReadBuffer : public std::streambuf {
    public:
        ReadBuffer(InputHandle input, uint64_t bytes) : input_(input), expected_(bytes) {}
    protected:
        int_type underflow() override {
            if (gptr() && gptr() < egptr()) return traits_type::to_int_type(*gptr());
            const auto remaining = expected_ - read_;
            const auto request = static_cast<size_t>(remaining >= sizeof(block_) ? sizeof(block_) : remaining + 1);
#ifdef _WIN32
            DWORD received = 0;
            if (!ReadFile(input_, block_, static_cast<DWORD>(request), &received, nullptr))
                throw std::ios_base::failure("Inspection input read failed");
#else
            ssize_t received;
            do { received = ::read(input_, block_, request); } while (received < 0 && errno == EINTR);
            if (received < 0) throw std::ios_base::failure("Inspection input read failed");
#endif
            if (!received) return traits_type::eof();
            if (static_cast<uint64_t>(received) > remaining) throw std::ios_base::failure("Inspection input grew");
            read_ += static_cast<uint64_t>(received);
            setg(block_, block_, block_ + received);
            return traits_type::to_int_type(*gptr());
        }
    private:
        InputHandle input_;
        uint64_t expected_, read_ = 0;
        char block_[65536];
    };
    RetainedInput handle_;
    FileStamp before_;
};

// The retained ancestry binds native lookups to the inspected directories.
// Directory read handles and Windows sharing do not establish writer termination.
class InspectionDirectory {
public:
    bool open(const fs::path& configured, const std::function<void()>& check) {
        std::error_code error;
        original_ = fs::absolute(configured, error);
        if (error) return false;
        path_ = fs::canonical(original_, error);
        if (error) return false;
#ifdef _WIN32
        const auto root_name = path_.root_name().native();
        if (root_name.size() != 2 || root_name[1] != L':') return false;
#endif
        fs::path current = path_.root_path();
        if (!append(current, {}, nullptr)) return false;
        for (const auto& part : path_.relative_path()) {
            check();
            current /= part;
            const auto parent = levels_.back().handle.value;
            if (!append(current, part, &parent)) return false;
        }
        return recheck();
    }
    const fs::path& path() const { return path_; }
    FileState status(const std::string& name, FileStamp& result) const {
        if (!safe_leaf(name)) return FileState::invalid;
#ifdef _WIN32
        const auto path = path_ / fs::u8path(name);
        RetainedInput handle(CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, nullptr));
        if (handle.value == invalid_input) return GetLastError() == ERROR_FILE_NOT_FOUND ? FileState::absent : FileState::invalid;
        return input_stamp(handle.value, false, result) ? FileState::present : FileState::invalid;
#else
        struct stat info{};
        if (::fstatat(levels_.back().handle.value, name.c_str(), &info, AT_SYMLINK_NOFOLLOW) != 0)
            return errno == ENOENT ? FileState::absent : FileState::invalid;
        return stat_stamp(info, false, result) ? FileState::present : FileState::invalid;
#endif
    }
    std::shared_ptr<InspectionInput> input(const std::string& name, const std::function<void()>& check,
                                          FileState& state, bool& changed) const {
        FileStamp before;
        state = status(name, before);
        changed = false;
        if (state != FileState::present) return {};
        check();
#ifdef _WIN32
        const auto path = path_ / fs::u8path(name);
        const auto handle = CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
#else
        const auto handle = ::openat(levels_.back().handle.value, name.c_str(), O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC);
#endif
        FileStamp opened;
        const bool verified = handle != invalid_input && input_stamp(handle, false, opened);
        if (!verified || !(opened == before)) {
            close_input(handle);
            FileStamp current;
            changed = verified || status(name, current) != FileState::present || !(current == before);
            state = FileState::invalid;
            return {};
        }
        RetainedInput retained(handle);
        auto result = std::make_shared<InspectionInput>(handle, opened);
        retained.value = invalid_input;
        return result;
    }
    bool recheck() const {
        for (size_t i = 0; i < levels_.size(); ++i) {
            const auto& level = levels_[i];
            FileStamp retained, current;
            if (!input_stamp(level.handle.value, true, retained) || !retained.same_identity(level.stamp)) return false;
#ifdef _WIN32
            RetainedInput handle(CreateFileW(level.path.c_str(), FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
            if (handle.value == invalid_input || !input_stamp(handle.value, true, current)) return false;
#else
            if (i == 0) current = retained;
            else {
                struct stat info{};
                if (::fstatat(levels_[i - 1].handle.value, level.name.c_str(), &info, AT_SYMLINK_NOFOLLOW) != 0 ||
                    !stat_stamp(info, true, current)) return false;
            }
#endif
            if (!current.same_identity(level.stamp)) return false;
        }
        std::error_code error;
        return fs::canonical(original_, error) == path_ && !error;
    }
    bool enumerate(const std::function<bool(const std::string&)>& visit) const {
#ifdef _WIN32
        WIN32_FIND_DATAW data{};
        const auto search = path_ / L"*";
        const auto handle = FindFirstFileW(search.c_str(), &data);
        if (handle == INVALID_HANDLE_VALUE) return GetLastError() == ERROR_FILE_NOT_FOUND;
        struct Cleanup { HANDLE handle; ~Cleanup() { FindClose(handle); } } cleanup{handle};
        do {
            const std::wstring name(data.cFileName);
            if (name != L"." && name != L".." && !visit(fs::path(name).u8string())) return true;
        } while (FindNextFileW(handle, &data));
        return GetLastError() == ERROR_NO_MORE_FILES;
#else
        const auto fd = ::openat(levels_.back().handle.value, ".", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
        if (fd < 0) return false;
        const auto directory = ::fdopendir(fd);
        if (!directory) { ::close(fd); return false; }
        struct Cleanup { DIR* directory; ~Cleanup() { ::closedir(directory); } } cleanup{directory};
        while (true) {
            errno = 0;
            const auto entry = ::readdir(directory);
            if (!entry) return errno == 0;
            const std::string name(entry->d_name);
            if (name != "." && name != ".." && !visit(name)) return true;
        }
#endif
    }

private:
    struct Level {
        RetainedInput handle;
        FileStamp stamp;
        fs::path path, name;
    };
    fs::path original_, path_;
    std::vector<Level> levels_;
    static bool safe_leaf(const std::string& name) {
        return !name.empty() && name != "." && name != ".." && name.find('\0') == std::string::npos && name.find_first_of("/\\") == std::string::npos
#ifdef _WIN32
            && name.find_first_of(":*?<>|\"") == std::string::npos && name.back() != '.' && name.back() != ' '
#endif
            ;
    }
    bool append(const fs::path& path, const fs::path& name, const InputHandle* parent) {
#ifdef _WIN32
        (void)parent;
        // Anchor ancestor names without excluding publication of child files.
        const auto handle = CreateFileW(path.c_str(), FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
#else
        const auto handle = parent ? ::openat(*parent, name.c_str(), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) :
            ::open(path.c_str(), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
#endif
        FileStamp stamp;
        if (handle == invalid_input || !input_stamp(handle, true, stamp)) { close_input(handle); return false; }
        levels_.push_back({RetainedInput(handle), stamp, path, name});
        return true;
    }
};

class Inspection {
public:
    explicit Inspection(const SnapshotInspectionOptions& options)
        : options_(options), deadline_(std::chrono::steady_clock::now() + options.timeout) {}

    json run(const fs::path& state_file, const fs::path& configured_root) {
        try {
            check_stop();
            if (state_file.empty()) { issue("state_file_identity_unverified"); return result_; }
            if (configured_root.empty()) { issue("snapshot_root_unavailable"); return result_; }
            std::error_code error;
            const auto absolute_state = fs::absolute(state_file, error);
            if (error || !state_parent_.open(absolute_state.parent_path(), [&] { check_stop(); })) {
                issue("state_directory_unavailable"); return result_;
            }
            state_name_ = absolute_state.filename().u8string();
            FileState state_status;
            bool state_changed;
            state_input_ = state_parent_.input(state_name_, [&] { check_stop(); }, state_status, state_changed);
            if (!state_input_) { issue(state_changed ? "state_changed_during_inspection" : "state_file_identity_unverified"); return result_; }
            if (state_input_->before().bytes > maximum_state_bytes) { issue("state_size_excessive"); return result_; }
            const auto content = read_text(*state_input_, "state_read_incomplete", "state_changed_during_inspection");
            const auto state = parse(content, "state_json_invalid", "state_structure_invalid");
            result_["source_sha256"] = utils::string_sha256(content);
            if (state.is_discarded()) return result_;
            if (!snapshot_metadata::valid_state_header(state)) {
                issue("state_schema_unknown"); return result_;
            }
            if (!snapshot_root_.open(configured_root, [&] { check_stop(); })) {
                issue("snapshot_root_unavailable"); return result_;
            }
            const auto& state_directory = state_parent_.path();
            const auto& snapshots = inventory(state, "snapshots");
            const auto& cleanup = inventory(state, "snapshot_cleanup");
            const auto& writes = inventory(state, "snapshot_writes");
            const auto& preparations = inventory(state, "preparations");
            if (!record_budget(snapshots.size()) || !record_budget(cleanup.size()) ||
                !record_budget(writes.size()) || !record_budget(preparations.size())) return result_;
            binding(state, state_directory, !snapshots.empty() || !cleanup.empty() || !writes.empty());
            for (const auto& [id, entry] : snapshots.items()) {
                check_stop();
                if (!snapshot_metadata::valid_saved(id, entry)) { issue("saved_snapshot_schema_invalid", id); continue; }
                const auto publication = writes.find(id + ".pending");
                if (publication != writes.end() && publication->is_object() && publication->value("state", json()) == "publishing")
                    issue("publication_conflicts_with_saved_inventory", id);
                verify(id + ".bin", entry, false, id);
                ++saved_;
            }
            for (const auto& [id, entry] : cleanup.items()) {
                check_stop();
                ++effects_;
                if (!snapshot_metadata::valid_cleanup_key(id) ||
                    !snapshot_metadata::valid_saved(id.substr(0, 64), entry)) {
                    issue("cleanup_metadata_invalid", id); continue;
                }
                if (id.size() == 64 && snapshots.contains(id)) issue("cleanup_conflicts_with_saved_snapshot", id);
                verify(id + ".bin", entry, true, id);
            }
            for (const auto& [pending, entry] : writes.items()) {
                check_stop();
                ++writers_;
                ++effects_;
                if (!snapshot_metadata::valid_write(pending, entry)) { issue("write_intent_schema_invalid", pending); continue; }
                const auto id = pending.substr(0, 64);
                const auto phase = entry["state"].get<std::string>();
                if (!entry.contains("files") || phase == "publishing") explained_.insert(pending);
                if (entry.contains("files"))
                    for (const auto& item : entry["files"].items()) explained_.insert(item.key());
                if (phase == "publishing") {
                    if (snapshots.contains(id)) issue("publication_conflicts_with_saved_inventory", pending);
                    if (cleanup.contains(id) || cleanup.contains(id + ".previous"))
                        issue("publication_conflicts_with_cleanup", pending);
                    publication(pending, entry);
                } else {
                    // A retained cleanup journal may describe partial, unsealed output.
                    // Its byte values are reservations, not content checksums.
                    if (!entry.contains("files")) observe(pending, false);
                    if (entry.contains("files")) {
                        for (const auto& item : entry["files"].items()) {
                            if ((item.key() == id + ".bin" && (snapshots.contains(id) || cleanup.contains(id))) ||
                                (item.key() == id + ".previous.bin" && cleanup.contains(id + ".previous")))
                                issue("write_files_conflict_with_inventory", pending);
                            observe(item.key(), false);
                        }
                    }
                }
            }
            writers(state, preparations);
            scan();
            recheck_observations();
            check_stop();
            FileStamp after;
            if (!state_parent_.recheck() || state_parent_.status(state_name_, after) != FileState::present ||
                !(after == state_input_->before()) || !state_input_->unchanged() ||
                read_text(*state_input_, "state_read_incomplete", "state_changed_during_inspection") != content)
                issue("state_changed_during_inspection");
            if (!snapshot_root_.recheck()) issue("snapshot_root_changed_during_inspection");
        } catch (const Stopped&) {
        } catch (const fs::filesystem_error&) {
            issue("inspection_io_failed");
        } catch (const json::exception&) {
            issue("state_structure_invalid");
        }
        result_["saved_snapshots"] = saved_;
        result_["pending_effects"] = effects_;
        result_["unverified_writers"] = writers_;
        return result_;
    }

private:
    struct Observation {
        FileState state = FileState::invalid;
        uint64_t bytes = 0;
        std::string checksum;
        std::shared_ptr<InspectionInput> input;
    };
    const SnapshotInspectionOptions& options_;
    std::chrono::steady_clock::time_point deadline_;
    InspectionDirectory state_parent_, snapshot_root_;
    std::string state_name_;
    std::shared_ptr<InspectionInput> state_input_, marker_input_;
    bool marker_checked_ = false, marker_absent_ = false;
    uint64_t hash_bytes_ = 0;
    size_t records_ = 0, saved_ = 0, effects_ = 0, writers_ = 0;
    std::set<std::string> explained_;
    std::map<std::string, Observation> observations_;
    const json empty_ = json::object();
    json result_ = {{"schema_version", 1}, {"read_only", true}, {"validation_complete", true},
        {"application_authorized", false}, {"writer_termination_verified", false},
        {"consistency", "read_only_observation_not_a_writer_fence"},
        {"saved_snapshots", 0}, {"pending_effects", 0}, {"unverified_writers", 0}, {"issues", json::array()}};

    void issue(const char* code, const std::string& record = {}) {
        result_["validation_complete"] = false;
        auto& rows = result_["issues"];
        if (rows.size() >= options_.maximum_issues) {
            result_["issues_truncated"] = true;
            return;
        }
        json row = {{"code", code}};
        if (!record.empty()) row["record_digest"] = utils::string_sha256(record);
        rows.push_back(std::move(row));
    }
    void check_stop() {
        if (options_.cancelled && options_.cancelled()) { issue("inspection_cancelled"); throw Stopped{}; }
        if (std::chrono::steady_clock::now() >= deadline_) { issue("inspection_deadline"); throw Stopped{}; }
    }
    bool record_budget(size_t count) {
        if (count > options_.maximum_records - records_) { issue("record_budget_exhausted"); return false; }
        records_ += count;
        return true;
    }
    const json& inventory(const json& state, const char* name) {
        const auto found = state.find(name);
        if (found == state.end()) return empty_;
        if (!found->is_object()) { issue("inventory_schema_invalid", name); return empty_; }
        return *found;
    }
    std::string read_text(InspectionInput& file, const char* failure, const char* changed) {
        check_stop();
        auto buffer = file.rewind();
        if (!buffer) { issue(failure); throw Stopped{}; }
        std::istream input(buffer.get());
        std::string content(static_cast<size_t>(file.before().bytes), '\0');
        size_t offset = 0;
        while (offset < content.size()) {
            check_stop();
            const auto count = (std::min)(size_t(65536), content.size() - offset);
            if (!input.read(content.data() + offset, static_cast<std::streamsize>(count))) {
                issue(file.unchanged() ? failure : changed); throw Stopped{};
            }
            offset += count;
        }
        if (!input || input.peek() != std::char_traits<char>::eof() || input.bad() || !file.unchanged()) {
            issue(file.unchanged() ? failure : changed); throw Stopped{};
        }
        return content;
    }
    json parse(const std::string& content, const char* syntax, const char* structure) {
        size_t events = 0;
        std::map<int, std::set<std::string>> keys;
        const json::parser_callback_t callback = [&](int depth, json::parse_event_t event, json& parsed) {
            check_stop();
            if (depth > int(maximum_json_depth) || ++events > maximum_json_events) throw InvalidStructure{};
            if (event == json::parse_event_t::object_start) keys[depth + 1].clear();
            if (event == json::parse_event_t::object_end) keys.erase(depth + 1);
            if (event == json::parse_event_t::key && !keys[depth].insert(parsed.get<std::string>()).second)
                throw InvalidStructure{};
            return true;
        };
        try {
            auto parsed = json::parse(content, callback, false);
            if (parsed.is_discarded()) issue(syntax);
            return parsed;
        } catch (const InvalidStructure&) { issue(structure); return json(json::value_t::discarded); }
    }
    void binding(const json& state, const fs::path& state_directory, bool has_records) {
        const auto found = state.find("snapshot_store");
        const std::string marker = ".juiced-snapshot-store.json";
        FileStamp initial;
        const bool absent = snapshot_root_.status(marker, initial) == FileState::absent;
        marker_checked_ = true;
        marker_absent_ = absent;
        if (found == state.end()) {
            marker_checked_ = absent;
            if (has_records) issue("snapshot_binding_missing");
            if (!absent) issue("snapshot_marker_without_state_binding");
            return;
        }
        if (!snapshot_metadata::valid_binding(*found)) { marker_checked_ = false; issue("snapshot_binding_schema_invalid"); return; }
        const auto& value = *found;
        if (value["directory"] != snapshot_root_.path().u8string()) issue("snapshot_root_binding_mismatch");
        if (value["state_directory"] != state_directory.u8string()) issue("state_directory_binding_mismatch");
        if (!value["ready"].get<bool>() && has_records) issue("unfinished_binding_has_snapshot_records");
        if (absent) {
            if (value["ready"].get<bool>()) issue("committed_snapshot_marker_missing");
            return;
        }
        FileState marker_status;
        bool changed;
        marker_input_ = snapshot_root_.input(marker, [&] { check_stop(); }, marker_status, changed);
        if (!marker_input_) {
            marker_checked_ = false;
            issue(changed ? "snapshot_marker_changed_during_inspection" : "snapshot_marker_identity_unverified"); return;
        }
        if (!(marker_input_->before() == initial)) issue("snapshot_marker_changed_during_inspection");
        if (marker_input_->before().bytes > 4096) { issue("snapshot_marker_size_invalid"); return; }
        const auto observed = parse(read_text(*marker_input_, "snapshot_marker_unreadable", "snapshot_marker_changed_during_inspection"),
            "snapshot_marker_json_invalid", "snapshot_marker_structure_invalid");
        auto expected = value;
        expected.erase("ready");
        if (!observed.is_discarded() && observed != expected) issue("snapshot_marker_mismatch");
    }
    Observation observe(const std::string& name, bool hash) {
        check_stop();
        explained_.insert(name);
        const auto cached = observations_.find(name);
        Observation observed;
        if (cached != observations_.end()) observed = cached->second;
        else {
            bool changed;
            observed.input = snapshot_root_.input(name, [&] { check_stop(); }, observed.state, changed);
            if (!observed.input && observed.state != FileState::absent)
                issue(changed ? "snapshot_changed_during_inspection" : "snapshot_file_identity_unverified", name);
            if (observed.input) observed.bytes = observed.input->before().bytes;
        }
        if (hash && observed.state == FileState::present && observed.checksum.empty()) {
            if (observed.bytes > options_.maximum_hash_bytes - hash_bytes_) {
                issue("hash_budget_exhausted", name); throw Stopped{};
            }
            hash_bytes_ += observed.bytes;
            try {
                const auto buffer = observed.input->rewind();
                if (!buffer) throw std::ios_base::failure("Cannot rewind inspection input");
                std::istream input(buffer.get());
                observed.checksum = utils::content_sha256(input, [&] { check_stop(); return false; });
            } catch (const Stopped&) { throw; }
            catch (const std::exception&) {
                issue(observed.input->unchanged() ? "snapshot_read_failed" : "snapshot_changed_during_inspection", name);
                observed.state = FileState::invalid;
            }
            if (!observed.input->unchanged()) {
                issue("snapshot_changed_during_inspection", name);
                observed.state = FileState::invalid;
            }
        }
        observations_[name] = observed;
        return observed;
    }
    static bool matches(const Observation& observed, const json& metadata) {
        return observed.state == FileState::present && checksum_metadata(metadata) &&
            observed.bytes == metadata["bytes"].get<uint64_t>() && observed.checksum == metadata["checksum"];
    }
    void verify(const std::string& name, const json& entry, bool optional, const std::string& record) {
        auto observed = observe(name, false);
        if (observed.state == FileState::absent) {
            if (!optional) issue("snapshot_file_missing", record);
        } else if (observed.state == FileState::present) {
            if (observed.bytes != entry["bytes"].get<uint64_t>()) issue("snapshot_size_mismatch", record);
            else {
                observed = observe(name, true);
                if (observed.state == FileState::present && !matches(observed, entry)) issue("snapshot_checksum_mismatch", record);
            }
        }
    }
    Observation publication_file(const std::string& name, const json& candidate, const json& previous) {
        const auto observed = observe(name, false);
        if (observed.state == FileState::present &&
            (observed.bytes == candidate["bytes"].get<uint64_t>() ||
                (!previous.empty() && observed.bytes == previous["bytes"].get<uint64_t>())))
            return observe(name, true);
        return observed;
    }
    void publication(const std::string& pending, const json& intent) {
        const auto id = pending.substr(0, 64);
        const auto& previous = intent["previous"];
        const auto& candidate = intent["candidate"];
        const auto source = publication_file(pending, candidate, json::object());
        const auto destination = publication_file(id + ".bin", candidate, previous);
        const auto backup = publication_file(id + ".previous.bin", previous.empty() ? candidate : previous, json::object());
        const bool prior = !previous.empty();
        const bool before_swap = matches(source, candidate) && backup.state == FileState::absent &&
            (prior ? matches(destination, previous) : destination.state == FileState::absent);
        const bool after_backup = matches(source, candidate) && destination.state == FileState::absent &&
            prior && matches(backup, previous);
        const bool after_swap = source.state == FileState::absent && matches(destination, candidate) &&
            (prior ? matches(backup, previous) : backup.state == FileState::absent);
        if (!before_swap && !after_backup && !after_swap) issue("publication_files_do_not_match_intent", pending);
    }
    void recheck_observations() {
        for (const auto& [name, observed] : observations_) {
            check_stop();
            if (observed.state == FileState::invalid) continue;
            FileStamp after;
            const auto state = snapshot_root_.status(name, after);
            if (observed.state == FileState::absent ? state != FileState::absent :
                (state != FileState::present || !(after == observed.input->before()) || !observed.input->unchanged()))
                issue("snapshot_changed_during_inspection", name);
        }
        if (marker_checked_) {
            FileStamp after;
            const auto state = snapshot_root_.status(".juiced-snapshot-store.json", after);
            if (marker_absent_ ? state != FileState::absent :
                (state != FileState::present || !marker_input_ || !(after == marker_input_->before()) || !marker_input_->unchanged()))
                issue("snapshot_marker_changed_during_inspection");
        }
    }
    void writers(const json& state, const json& preparations) {
        if (state.contains("voice_writer")) {
            const auto& voice = state["voice_writer"];
            if (!voice.is_object() || voice.size() != 3 || !voice.contains("owner") || !text(voice["owner"]) ||
                !voice.contains("pending") || !voice["pending"].is_boolean() ||
                voice.value("state", json()) != (voice["pending"].get<bool>() ? "dispatching" : "stopped")) {
                ++writers_; issue("voice_intent_schema_invalid");
            } else if (voice["pending"].get<bool>()) ++writers_;
        }
        for (const auto& [id, entry] : preparations.items()) {
            check_stop();
            if (!entry.is_object() || !entry.contains("pending") || !entry["pending"].is_boolean() ||
                !entry.contains("writer_state") || !entry["writer_state"].is_string() ||
                !entry.contains("model") || !text(entry["model"], 512) ||
                (entry.contains("cleanup_pending") && !entry["cleanup_pending"].is_boolean())) {
                ++writers_; issue("preparation_writer_unknown", id); continue;
            }
            const auto writer = entry["writer_state"].get<std::string>();
            const bool queued = writer == "queued" && entry["pending"] == true && !entry.value("cleanup_pending", false);
            const auto phase = entry.value("phase", json());
            const bool terminal = phase == "complete" || phase == "cancelled" || phase == "failed" || phase == "interrupted";
            const bool stopped = writer == "stopped" && entry["pending"] == false && terminal && !entry.value("cleanup_pending", false);
            if (!queued && !stopped) {
                ++writers_;
                if (writer != "queued" && writer != "stopped" && writer != "dispatching") issue("preparation_writer_unknown", id);
            }
        }
    }
    void scan() {
        size_t count = 0;
        const auto complete = snapshot_root_.enumerate([&](const std::string& name) {
            check_stop();
            if (++count > options_.maximum_files) { issue("file_budget_exhausted"); return false; }
            FileStamp observed;
            if (name == ".juiced-store.lock" || name == ".juiced-snapshot-store.json") {
                if (snapshot_root_.status(name, observed) != FileState::present) issue("control_file_identity_unverified", name);
            } else {
                if (!explained_.count(name)) issue("unexplained_snapshot_file", name);
                if (snapshot_root_.status(name, observed) != FileState::present) issue("snapshot_file_identity_unverified", name);
            }
            return true;
        });
        if (!complete) issue("snapshot_inventory_unreadable");
    }
};
}

namespace snapshot_metadata {
bool valid_id(const std::string& id) {
    return id.size() == 64 && id.find_first_not_of("0123456789abcdef") == std::string::npos;
}
bool valid_state_header(const json& state) {
    return state.is_object() && state.contains("version") && natural(state["version"]) && state["version"] == 1 &&
        (!state.contains("revision") || natural(state["revision"]));
}
bool valid_binding(const json& binding) {
    return binding.is_object() && binding.size() == 5 && binding.value("version", json()) == 1 &&
        binding.contains("version") && natural(binding["version"]) &&
        binding.contains("ready") && binding["ready"].is_boolean() &&
        binding.contains("id") && text(binding["id"]) &&
        binding.contains("state_directory") && text(binding["state_directory"], 32768) &&
        binding.contains("directory") && text(binding["directory"], 32768);
}
bool valid_saved(const std::string& id, const json& entry) {
    return valid_id(id) && checksum_metadata(entry) && (!entry.contains("id") || entry["id"] == id) &&
        entry.contains("profile") && entry["profile"].is_string() && valid_id(entry["profile"].get_ref<const std::string&>()) &&
        entry.contains("model") && text(entry["model"], 512) && entry.value("state", json()) == "saved" &&
        (!entry.contains("session") || text(entry["session"], 512)) &&
        (!entry.contains("tokens") || natural(entry["tokens"])) &&
        (!entry.contains("prefix_messages") || natural(entry["prefix_messages"])) &&
        (!entry.contains("prefix_digest") || (entry["prefix_digest"].is_string() && valid_id(entry["prefix_digest"].get_ref<const std::string&>()))) &&
        (!entry.contains("last_used") || natural(entry["last_used"])) &&
        (!entry.contains("retained") || entry["retained"].is_boolean());
}
bool valid_cleanup_key(const std::string& key) {
    return valid_id(key) || (key.size() == 73 && key.substr(64) == ".previous" && valid_id(key.substr(0, 64)));
}
bool valid_write(const std::string& pending, const json& entry) {
    if (pending.size() != 72 || pending.substr(64) != ".pending" || !valid_id(pending.substr(0, 64)) ||
        !entry.is_object() || !entry.contains("owner") || !text(entry["owner"]) ||
        !entry.contains("generation") || !natural(entry["generation"]) || entry["generation"] == 0 ||
        !entry.contains("bytes") || !natural(entry["bytes"]) || entry["bytes"] == 0 ||
        (entry.contains("model") && !text(entry["model"], 512)) ||
        (entry.contains("profile") && (!entry["profile"].is_string() ||
            (!entry["profile"].get_ref<const std::string&>().empty() && !valid_id(entry["profile"].get_ref<const std::string&>()))))) return false;
    const auto phase = entry.value("state", json());
    if (phase != "writing" && phase != "uncertain" && phase != "publishing" && phase != "cleanup") return false;
    const auto id = pending.substr(0, 64);
    if (entry.contains("files")) {
        if (!entry["files"].is_object() || entry["files"].size() > 3) return false;
        for (const auto& item : entry["files"].items())
            if ((item.key() != pending && item.key() != id + ".bin" && item.key() != id + ".previous.bin") || !natural(item.value())) return false;
    }
    if (phase == "publishing") {
        if (entry.contains("files") || entry.value("destination", json()) != id + ".bin" || entry.value("backup", json()) != id + ".previous.bin" ||
            !entry.contains("candidate") || !valid_saved(id, entry["candidate"]) || entry["candidate"].value("id", json()) != id ||
            entry["candidate"]["bytes"].get<uint64_t>() > entry["bytes"].get<uint64_t>() ||
            (entry.contains("model") && entry["model"] != entry["candidate"]["model"]) ||
            (entry.contains("profile") && entry["profile"] != entry["candidate"]["profile"]) ||
            !entry.contains("previous") || !entry["previous"].is_object() ||
            (!entry["previous"].empty() && !valid_saved(id, entry["previous"]))) return false;
    }
    return true;
}
}

json SnapshotRecovery::inspect_file(const fs::path& state_file, const fs::path& root, const SnapshotInspectionOptions& options) {
    if (options.timeout <= std::chrono::milliseconds::zero() || options.timeout > std::chrono::minutes(5) ||
        !options.maximum_hash_bytes || options.maximum_hash_bytes > (1ULL << 40) ||
        !options.maximum_records || options.maximum_records > 16384 ||
        !options.maximum_files || options.maximum_files > 16384 || !options.maximum_issues || options.maximum_issues > 1024)
        throw std::invalid_argument("Invalid snapshot inspection limits");
    return Inspection(options).run(state_file, root);
}
}
