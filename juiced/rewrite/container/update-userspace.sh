#!/bin/sh
set -eu

if [ "$(id -u)" != 0 ]; then
    echo "juiced-update-userspace needs root: podman exec -u 0 <container> juiced-update-userspace" >&2
    exit 77
fi

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get upgrade -y --no-install-recommends
apt-get install -y --no-install-recommends --only-upgrade \
    libvulkan1 vulkan-tools libegl1 libxext6 libdrm2 libdrm-amdgpu1 libgomp1 libnuma1 ca-certificates
apt-get install -y --no-install-recommends -t trixie-backports mesa-vulkan-drivers
rm -f /usr/share/vulkan/icd.d/lvp_icd*.json
rm -rf /var/lib/apt/lists/*
dpkg-query -W -f '${Package} ${Version}\n' mesa-vulkan-drivers libvulkan1 libdrm2 libdrm-amdgpu1
echo "Engines started after this point use the updated userspace; reload a model to apply it." >&2
