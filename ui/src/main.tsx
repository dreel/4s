import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App, installUndoKeys } from "./App";
import { installAssistantKeys } from "./assistant";
import "./index.css";
import { client } from "./store";

client.connect();
installUndoKeys();
installAssistantKeys();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
