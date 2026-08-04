import "./App.css";
import { useStore } from "./store";
import { detectView } from "./transport";
import { ControlView } from "./views/ControlView";
import { OutputView } from "./views/OutputView";

// Decided once at startup: native windows by their Tauri label, web clients
// by URL path (/control vs /output).
const view = detectView();

function App() {
  const store = useStore();
  return view === "output" ? (
    <OutputView store={store} />
  ) : (
    <ControlView store={store} />
  );
}

export default App;
