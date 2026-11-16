import { createRoot } from "react-dom/client";
import { App } from "./App";
import { applyTheme, readTheme } from "./theme";

applyTheme(readTheme());
createRoot(document.getElementById("root")!).render(<App />);
