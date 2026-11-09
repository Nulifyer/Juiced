#include "amd_metrics_windows.h"
#include "nvidia_metrics.h"
#include <lemon/system_metrics_platform.h>
#include <windows.h>
#include <cmath>
#include <vector>

namespace lemon {

class WindowsMetricsPlatform : public SystemMetricsPlatform {
public:
    const char* get_platform_name() const override {
        return "Windows";
    }

    double get_cpu_usage(std::mutex& cpu_stats_mutex,
                        uint64_t& last_total,
                        uint64_t& last_total_idle) override {
        std::lock_guard<std::mutex> lock(cpu_stats_mutex);

        FILETIME idle_time, kernel_time, user_time;
        if (!GetSystemTimes(&idle_time, &kernel_time, &user_time)) {
            return -1.0;
        }

        // Convert FILETIME to uint64_t (100-nanosecond intervals)
        auto filetime_to_uint64 = [](const FILETIME& ft) -> uint64_t {
            return (static_cast<uint64_t>(ft.dwHighDateTime) << 32) | ft.dwLowDateTime;
        };

        uint64_t idle = filetime_to_uint64(idle_time);
        uint64_t kernel = filetime_to_uint64(kernel_time); // Includes idle time
        uint64_t user = filetime_to_uint64(user_time);

        // Kernel time includes idle time, so subtract it to get actual kernel time
        uint64_t total = kernel + user;
        uint64_t total_idle = idle;

        if (last_total > 0) {
            uint64_t idle_diff = total_idle - last_total_idle;
            uint64_t total_diff = total - last_total;

            last_total_idle = total_idle;
            last_total = total;

            if (total_diff > 0) {
                return ((total_diff - idle_diff) * 100.0) / total_diff;
            }
        }

        last_total_idle = total_idle;
        last_total = total;
        return 0.0; // First call, no delta yet
    }

    double get_memory_usage_gb() override {
        MEMORYSTATUSEX memInfo;
        memInfo.dwLength = sizeof(MEMORYSTATUSEX);
        if (GlobalMemoryStatusEx(&memInfo)) {
            double used_gb = (memInfo.ullTotalPhys - memInfo.ullAvailPhys) / (1024.0 * 1024.0 * 1024.0);
            return std::round(used_gb * 10.0) / 10.0;
        }
        return 0.0;
    }

    double get_memory_total_gb() override {
        MEMORYSTATUSEX memInfo;
        memInfo.dwLength = sizeof(MEMORYSTATUSEX);
        if (GlobalMemoryStatusEx(&memInfo)) {
            return memInfo.ullTotalPhys / (1024.0 * 1024.0 * 1024.0);
        }
        return 0.0;
    }

    MemoryBudget get_memory_budget() override {
        MemoryBudget result;
        result.source = "GlobalMemoryStatusEx";
        MEMORYSTATUSEX sample{};
        sample.dwLength = sizeof(sample);
        if (!GlobalMemoryStatusEx(&sample)) return result;
        result.host_total_bytes = sample.ullTotalPhys;
        result.host_available_bytes = sample.ullAvailPhys;
        if (sample.ullTotalPageFile == 0 || sample.ullAvailPageFile > sample.ullTotalPageFile ||
            sample.ullTotalVirtual == 0 || sample.ullAvailVirtual > sample.ullTotalVirtual)
            return result;
        result.constraints.push_back({sample.ullTotalPageFile,
            sample.ullTotalPageFile - sample.ullAvailPageFile, "process-commit"});
        result.constraints.push_back({sample.ullTotalVirtual,
            sample.ullTotalVirtual - sample.ullAvailVirtual, "process-address-space"});
        BOOL in_job = FALSE;
        if (!IsProcessInJob(GetCurrentProcess(), nullptr, &in_job)) return result;
        // The immediate job does not establish every ancestor's memory limits.
        result.containment = in_job ? "windows-job-unqualified" : "windows-no-job";
        result.constraints_complete = !in_job;
        return result;
    }

    double get_gpu_usage() override {
        return get_system_gpu_metrics().gpu_percent;
    }

    double get_vram_usage_gb() override {
        return get_system_gpu_metrics().vram_used_gb;
    }

    SystemGpuMetrics get_system_gpu_metrics() override {
        auto devices = query_amd_gpu_metrics();
        auto nvidia = query_nvidia_gpu_metrics();
        devices.insert(devices.end(), nvidia.begin(), nvidia.end());
        return gpu_tracker_.update(devices);
    }

    double get_npu_utilization() override {
        // NPU monitoring not implemented for Windows
        return -1.0;
    }
private:
    GpuMetricsTracker gpu_tracker_;
};

std::unique_ptr<SystemMetricsPlatform> create_metrics_platform() {
    return std::make_unique<WindowsMetricsPlatform>();
}

} // namespace lemon
