# Retained conversion records

Local model conversion and quantization are retired. Use [published variants](variants.md)
to acquire compatible existing weights. New producer plans, starts and resume
requests return HTTP410 `feature_retired` through every native API prefix.

Existing model registrations, weights, job records and bounded output are retained.
The Runtime inspector shows previous jobs where they exist. Cancellation requests
cleanup; it does not prove process termination. Unknown prior writers remain
quarantined and protect their source and staging files. Never remove those files
manually to dismiss a cleanup error.

**Cache > Retained weights** shows reference-protected artifacts created by earlier
versions. Remove an unwanted registration through the normal model removal owner,
then review whether its unreferenced copy can be reclaimed. Feature retirement does
not delete or silently replace model weights.
