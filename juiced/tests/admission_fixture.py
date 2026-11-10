"""Bounded publication across admission changes for isolated server fixtures."""

import time


def publish_with_admission_retry(call, path, body, observations):
    started = time.monotonic()
    deadline = started + 10
    attempts = 0
    while True:
        remaining = deadline - time.monotonic()
        assert remaining > 0, ("Admission transition did not drain", attempts)
        status, headers, data = call(path, body, timeout=remaining)
        attempts += 1
        if status == 200:
            observations.append(
                {
                    "attempts": attempts,
                    "busy_rejections": attempts - 1,
                    "elapsed_seconds": time.monotonic() - started,
                }
            )
            return data
        error = data.get("error", {}) if isinstance(data, dict) else {}
        details = error.get("details", {}) if isinstance(error, dict) else {}
        code = (
            error.get("code", details.get("code")) if isinstance(error, dict) else None
        )
        assert status == 409 and code == "admission_configuration_busy", (status, data)
        remaining = deadline - time.monotonic()
        assert remaining > 0, ("Admission transition did not drain", attempts, data)
        delay = float(headers.get("Retry-After", details.get("retry_after", 1)))
        time.sleep(min(max(0.01, delay), remaining))


def configure_admission(call, settings, observations):
    return publish_with_admission_retry(
        call, "/internal/set", {"admission": settings}, observations
    )
