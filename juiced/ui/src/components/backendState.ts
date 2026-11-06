export const backendReady = (state: string) => ["installed", "update_available"].includes(state);
export const backendInstalled = (state: string) => backendReady(state) || state === "update_required";
export const backendUpdate = (state: string) => ["update_available", "update_required"].includes(state);
export const backendLabel = (state: string) => ({ installed: "Installed", update_available: "Update available", update_required: "Update required", installable: "Available to install" })[state] || "Unavailable";
