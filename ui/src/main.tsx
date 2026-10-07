import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App, installUndoKeys } from "./App";
import "./index.css";
import { client } from "./store";

client.connect();
installUndoKeys();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
