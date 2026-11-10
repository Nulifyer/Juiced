#include "lemon/resource_admission.h"

#include <cstdio>
#include <limits>

using lemon::ResourceAdmission;
using json = nlohmann::json;

int main() {
    int checks = 0, failures = 0;
    const auto check = [&](bool condition, const char* name) {
        ++checks;
        std::fprintf(stderr, "CHECK %d: %s\n", checks, name);
        if (!condition) { ++failures; std::fprintf(stderr, "FAIL: %s\n", name); }
    };
    ResourceAdmission::Work work;
    work.model = "Fixture-Q6";
    work.checkpoint = "publisher/fixture:Q6";
    work.options = {{"ctx_size", 4096}};
    work.operation = ResourceAdmission::Operation::Load;
    json profile = {{"checkpoint", work.checkpoint}, {"options", work.options},
        {"load_bytes", {{"uma", 500}}}, {"resident_bytes", {{"uma", 300}}},
        {"request_bytes", {{"uma", 100}}}, {"state_bytes", {{"uma", 200}}},
        {"source", "Controlled whole-weight/context/workspace ceilings"}};
    json settings = {{"mode", "legacy"},
        {"pools", {{"uma", {{"capacity_bytes", 1000}, {"essential_headroom_bytes", 100},
            {"source", "One shared CPU/GPU pool"}}}}},
        {"models", {{work.model, profile}}}};
    uint64_t available = 1000;
    unsigned samples = 0;
    ResourceAdmission admission([&](const std::string& pool) -> std::optional<uint64_t> {
        ++samples;
        return pool == "uma" ? std::optional<uint64_t>(available) : std::nullopt;
    });
    try {
        const auto before_unknown = admission.snapshot();
        const auto missing = admission.assess_load(work);
        check(missing["fit"] == "unknown" && missing["reason_code"] == "admission_profile_unknown" && samples == 0,
              "missing estimates remain unknown without probing or inventing zero memory");
        check(admission.snapshot() == before_unknown, "unknown suitability leaves admission state unchanged");
        admission.configure(settings);
        const auto before = admission.snapshot();
        const auto fits = admission.assess_load(work);
        check(fits["fit"] == "fits" && fits["reason_code"] == "within_declared_limits",
              "matched complete ceilings fit sampled physical headroom");
        check(fits["pools"].size() == 1 && fits["pools"]["uma"]["required_peak_bytes"] == 600,
              "whole weights plus one request and simultaneous state share one UMA peak");
        check(samples == 1, "each pool is sampled once across nominal and current decisions");
        check(fits["estimate_source"] == profile["source"] && !fits["reservation"].get<bool>() &&
              !fits["hardware_qualified"].get<bool>() && fits["configuration_revision"] == admission.policy().revision,
              "projection retains estimate provenance and revision without granting qualification or a reservation");
        check(admission.snapshot() == before, "successful suitability consumes no slots, reservations or counters");

        auto changed = work; changed.options["ctx_size"] = 8192;
        check(admission.assess_load(changed)["reason_code"] == "admission_profile_mismatch",
              "changed context invalidates the exact execution estimate");
        changed = work; changed.checkpoint = "publisher/fixture:Q4";
        check(admission.assess_load(changed)["fit"] == "unknown", "another quant cannot inherit the same model name's estimate");
        check(admission.snapshot() == before, "mismatch inspection does not count as a rejected admission");
        bool rejected = false;
        try { auto invalid = admission.acquire(changed); }
        catch (const lemon::AdmissionError& error) { rejected = error.code == "admission_profile_mismatch"; }
        check(rejected, "actual admission retains the same execution-profile mismatch rule");

        available = 699;
        check(admission.assess_load(work)["fit"] == "limited_fit", "current headroom shortfall stays distinct from impossible declared fit");
        available = 700;
        check(admission.assess_load(work)["fit"] == "fits", "the exact essential-headroom boundary remains usable");
        available = 1000;
        {
            auto resident_work = work; resident_work.generation = 7;
            auto resident = admission.acquire(resident_work);
            resident.started(); resident.retain_residency();
            auto candidate = work; candidate.generation = 8;
            const auto observed = admission.assess_load(candidate);
            check(observed["pools"]["uma"]["reserved_bytes"] == 300 && observed["fit"] == "fits",
                  "resident weights are charged once while assessing a distinct new worker");
            auto active = work; active.operation = ResourceAdmission::Operation::Request; active.generation = 7;
            auto request = admission.acquire(active);
            check(admission.assess_load(work)["fit"] == "limited_fit", "outstanding incremental work reduces the new-worker window");
        }
        check(admission.assess_load(work)["fit"] == "fits", "released reservations restore a truthful current-fit observation");
        auto small = settings; small["pools"]["uma"]["capacity_bytes"] = 699;
        admission.configure(small);
        check(admission.assess_load(work)["fit"] == "does_not_fit", "permanent physical ceiling shortfall is reported separately from current pressure");
        auto unknown = settings; unknown["pools"]["uma"]["capacity_bytes"] = nullptr;
        admission.configure(unknown);
        check(admission.assess_load(work)["fit"] == "unknown", "unknown physical capacity never becomes a fitting model");
        auto exhausted = settings; exhausted["pools"]["uma"]["capacity_bytes"] = 0;
        admission.configure(exhausted);
        check(admission.assess_load(work)["fit"] == "does_not_fit", "explicit zero capacity means exhausted rather than missing metadata");

        auto incomplete = settings; incomplete["models"][work.model].erase("request_bytes");
        admission.configure(incomplete);
        check(admission.assess_load(work)["reason_code"] == "resource_ceiling_incomplete",
              "weight and load estimates alone cannot establish serving fit");
        auto overflow = settings;
        for (const auto* key : {"load_bytes", "resident_bytes", "request_bytes", "state_bytes"})
            overflow["models"][work.model][key]["uma"] = std::numeric_limits<int64_t>::max();
        admission.configure(overflow);
        check(admission.assess_load(work)["reason_code"] == "resource_ceiling_overflow",
              "combined ceilings cannot wrap into false free capacity");

        ResourceAdmission unobserved;
        unobserved.configure(settings);
        check(unobserved.assess_load(work)["fit"] == "unknown", "a configured ceiling without current sampling is not observed host fit");
        ResourceAdmission failed_probe([](const std::string&) -> std::optional<uint64_t> { throw std::runtime_error("fixture probe failure"); });
        failed_probe.configure(settings);
        check(failed_probe.assess_load(work)["fit"] == "unknown", "failed read-only probes leave capacity unknown");
        auto multi = settings;
        multi["pools"]["device"] = {{"capacity_bytes", 1000}, {"source", "Independent device pool"}};
        for (const auto* key : {"load_bytes", "resident_bytes", "request_bytes"}) multi["models"][work.model][key]["device"] = 100;
        admission.configure(multi);
        check(admission.assess_load(work)["fit"] == "unknown", "an unobserved device pool cannot borrow host or UMA capacity");

        admission.configure(settings);
        admission.pause(true);
        const auto paused_before = admission.snapshot();
        const auto paused = admission.assess_load(work);
        check(paused["fit"] == "fits" && !paused["admission_open"].get<bool>() && paused["admission_blockers"] == json::array({"admission_paused"}),
              "temporary pause is separate from physical suitability");
        check(admission.snapshot() == paused_before, "inspection never resumes a paused server");
        admission.pause(false); admission.maintenance(true);
        check(admission.assess_load(work)["admission_blockers"] == json::array({"maintenance_requested"}), "maintenance remains authoritative without changing the fit estimate");
        admission.shutdown();
        const auto stopped = admission.assess_load(work);
        check(!stopped["admission_open"].get<bool>(), "suitability cannot reopen a stopped admission owner");
    } catch (const std::exception& error) {
        std::fprintf(stderr, "Unexpected suitability failure: %s\n", error.what());
        return 1;
    }
    std::fprintf(stderr, "Resource suitability: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
