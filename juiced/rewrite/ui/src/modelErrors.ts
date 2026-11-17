import { ApiError } from "./api";

/** Plain wording for the library and download failures the service reports by code. */
const messages: Record<string, string> = {
  artifact_revision_conflict: "Something else changed the library at the same time. Try again.",
  artifact_not_found: "That file or download is gone. Refresh, then try again.",
  artifact_busy: "A model or a download is still using this file.",
  artifact_limit: "Too many downloads at once. Wait for one to finish.",
  artifact_offline: "Offline mode is on, so nothing can be downloaded.",
  artifact_source_policy: "This source isn't allowed. Use a public HTTPS link.",
  artifact_source_changed: "The source file changed. Retry starts over.",
  artifact_network: "The connection dropped. Resume continues from the downloaded part.",
  artifact_job_failed: "The download didn't finish.",
  artifact_interrupted: "The download was interrupted. Resume it.",
  artifact_hash_mismatch: "The file didn't match its SHA256.",
  artifact_size_mismatch: "The file was bigger than its stated size or the free space.",
  artifact_publication_uncertain: "Saving the file may not have finished. Its space stays reserved.",
  artifact_corrupt: "The file failed its check on the server.",
  artifact_deadline: "Checking the files took too long. Add the model again.",
  artifact_io: "Storage failed on the server. Check its disk, then retry.",
  artifact_persistence: "Storage failed on the server. Check its disk, then retry.",
  storage_root_unavailable: "A storage folder moved or disappeared. Restore it, then retry.",
};

export const codeMessage = (code: string): string | undefined => messages[code];

/** Why the step after a download failed, in plain words. */
export const thenFailure = (then: { error: string | null; message: string | null }): string | null => (then.error ? messages[then.error] : undefined) ?? then.message;

export function plainError(failure: unknown, fallback: string): string {
  if (failure instanceof ApiError && messages[failure.code]) return messages[failure.code]!;
  return failure instanceof Error && failure.message ? failure.message : fallback;
}
