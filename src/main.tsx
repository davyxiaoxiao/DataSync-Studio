import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

// StrictMode is intentionally omitted: its double-invoked effects would issue
// duplicate database round trips during development.
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(<App />);
