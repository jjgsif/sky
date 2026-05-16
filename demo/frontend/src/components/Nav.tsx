import { NavLink } from "react-router-dom";

const links = [
  { to: "/", label: "Home", icon: "⌂" },
  { to: "/hello", label: "Hello", icon: "👋" },
  { to: "/health", label: "Health", icon: "♥" },
  { to: "/stream", label: "Stream", icon: "⚡" },
  { to: "/users", label: "Users", icon: "👤" },
  { to: "/upload", label: "Upload", icon: "^" },
];

export default function Nav() {
  return (
    <nav className="nav-sidebar">
      <div className="nav-brand">
        <span className="nav-logo">Sky</span>
        <span className="nav-tagline">demo</span>
      </div>
      <ul className="nav-links">
        {links.map(({ to, label, icon }) => (
          <li key={to}>
            <NavLink
              to={to}
              end={to === "/"}
              className={({ isActive }) =>
                `nav-link${isActive ? " nav-link--active" : ""}`
              }
            >
              <span className="nav-icon">{icon}</span>
              {label}
            </NavLink>
          </li>
        ))}
      </ul>
      <div className="nav-footer">
        <span>Sky + Vite</span>
      </div>
    </nav>
  );
}
