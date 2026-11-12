import type { Client } from "./api";
import { Downloaded } from "./Downloaded";
import { Downloading } from "./Downloading";
import { FindModels } from "./FindModels";
import { NativeImport } from "./NativeImport";

export type LibraryView = "search" | "downloaded" | "downloading" | "import";
export const libraryViews = [["search", "Search"], ["downloaded", "Downloaded"], ["downloading", "Downloading"], ["import", "Import"]] as const;

export function Library({ client, online, view, onView, onRegistered, onOpenModel }: { client: Client; online: boolean; view: LibraryView; onView: (view: LibraryView) => void; onRegistered: () => Promise<void>; onOpenModel: (id: string) => void }) {
  return <div className="library">
    <p className="page-intro muted">Model files on this machine. Add one to <button className="link-button" onClick={() => onOpenModel("")}>Models</button> to make it available to clients.</p>
    {view === "search" && <FindModels client={client} online={online} onRegistered={onRegistered} onOpenModel={onOpenModel} onDownloads={() => onView("downloading")} />}
    {view === "downloaded" && <Downloaded client={client} online={online} onRegistered={onRegistered} onOpenModel={onOpenModel} onSearch={() => onView("search")} />}
    {view === "downloading" && <Downloading client={client} online={online} onSearch={() => onView("search")} />}
    {view === "import" && <NativeImport client={client} online={online} onImported={onRegistered} />}
  </div>;
}
