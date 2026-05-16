import { BrowserRouter, Routes, Route } from "react-router-dom";
import Nav from "./components/Nav";
import HomePage from "./pages/HomePage";
import HelloPage from "./pages/HelloPage";
import HealthPage from "./pages/HealthPage";
import StreamPage from "./pages/StreamPage";
import UsersPage from "./pages/UsersPage";
import UploadPage from "./pages/UploadPage";
import "./App.css";

export default function App() {
  return (
    <BrowserRouter basename="/app">
      <div className="shell">
        <Nav />
        <main className="page-content">
          <Routes>
            <Route path="/" element={<HomePage />} />
            <Route path="/hello" element={<HelloPage />} />
            <Route path="/health" element={<HealthPage />} />
            <Route path="/stream" element={<StreamPage />} />
            <Route path="/users" element={<UsersPage />} />
            <Route path="/upload" element={<UploadPage />} />
          </Routes>
        </main>
      </div>
    </BrowserRouter>
  );
}
