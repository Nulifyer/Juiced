#pragma once
#include "lemon/backends/backend_descriptor.h"
namespace lemon::backends::parakeet {
inline const BackendDescriptor descriptor = {
    /*recipe*/ "parakeet",
    /*display_name*/ "Parakeet Redux",
    /*binary*/ "parakeet-server",
    /*config_section*/ "parakeet",
    /*default_device*/ DEVICE_CPU,
    /*slot_policy*/ SlotPolicy::Standard,
    /*selectable_backend*/ false,
    /*uses_ctx_size*/ false,
    /*dynamic_models*/ false,
    /*options*/
    {{"parakeet_threads", "--parakeet-threads", 4, "N", "CPU inference threads",
      "Parakeet Options"}},
    /*support*/
    {{"cpu",
      BackendTier::Experimental,
      BackendFormat::Native,
      {"linux"},
      {{"cpu", {"x86_64"}}},
      "x86_64 CPU"}},
    /*supported_modes*/ {"transcription"},
    /*required_checkpoints*/ {"main"},
    /*default_capabilities*/ {},
    /*web_display_name*/ "Parakeet Redux",
    /*rocm_channels*/ {},
    /*exposes_prometheus_metrics*/ false,
    /*rocm_requires_cwsr_fix*/ false,
    /*version_policy*/ VersionPolicy::Exact,
    /*self_manages_downloads*/ false,
    /*takes_args*/ false,
    /*arg_variants*/ {},
    /*bin_variants*/ {"cpu"},
};
}
