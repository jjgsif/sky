import { useState, useEffect } from "react";

interface User {
  id: string;
  name: string;
  email: string;
  role: "admin" | "member";
  createdAt: string;
}

interface ListResponse {
  items: User[];
  total: number;
  page: number;
  limit: number;
}

type UserForm = { name: string; email: string; role: "admin" | "member" };

function emptyForm(): UserForm {
  return { name: "", email: "", role: "member" };
}

export default function UsersPage() {
  const [users, setUsers] = useState<User[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [createForm, setCreateForm] = useState<UserForm>(emptyForm());
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  const [selected, setSelected] = useState<User | null>(null);
  const [editForm, setEditForm] = useState<UserForm>({ name: "", email: "", role: "member" });
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await fetch("/users?limit=50");
      if (!res.ok) throw new Error(`${res.status}`);
      const data = await res.json() as ListResponse;
      setUsers(data.items);
      setTotal(data.total);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => { load(); }, []);

  const createUser = async () => {
    if (!createForm.name || !createForm.email) return;
    setCreating(true);
    setCreateError(null);
    try {
      const res = await fetch("/users", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(createForm),
      });
      if (!res.ok) {
        const t = await res.text();
        setCreateError(`${res.status}: ${t}`);
        return;
      }
      setCreateForm(emptyForm());
      await load();
    } catch (e) {
      setCreateError(String(e));
    } finally {
      setCreating(false);
    }
  };

  const selectUser = (u: User) => {
    setSelected(u);
    setEditForm({ name: u.name, email: u.email, role: u.role });
  };

  const saveUser = async () => {
    if (!selected) return;
    setSaving(true);
    try {
      const res = await fetch(`/users/${selected.id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(editForm),
      });
      if (!res.ok) throw new Error(`${res.status}`);
      const updated = await res.json() as User;
      setSelected(updated);
      await load();
    } catch (e) {
      alert(String(e));
    } finally {
      setSaving(false);
    }
  };

  const deleteUser = async () => {
    if (!selected) return;
    if (!confirm(`Delete user "${selected.name}"?`)) return;
    setDeleting(true);
    try {
      const res = await fetch(`/users/${selected.id}`, { method: "DELETE" });
      if (!res.ok && res.status !== 204) throw new Error(`${res.status}`);
      setSelected(null);
      await load();
    } catch (e) {
      alert(String(e));
    } finally {
      setDeleting(false);
    }
  };

  return (
    <div className="page">
      <header className="page-header">
        <h1>Users CRUD</h1>
        <p className="subtitle">
          Full create / read / update / delete with Zod validation at the gateway.
          Data lives in-memory in the worker — restarts reset it.
        </p>
      </header>

      <div className="users-layout">
        {/* Create form */}
        <section className="card">
          <h2>Create user</h2>
          <div className="field-group">
            <label className="field-label">Name</label>
            <input
              value={createForm.name}
              onChange={(e) => setCreateForm((f) => ({ ...f, name: e.target.value }))}
              placeholder="Alice"
            />
          </div>
          <div className="field-group">
            <label className="field-label">Email</label>
            <input
              type="email"
              value={createForm.email}
              onChange={(e) => setCreateForm((f) => ({ ...f, email: e.target.value }))}
              placeholder="alice@example.com"
            />
          </div>
          <div className="field-group">
            <label className="field-label">Role</label>
            <select
              value={createForm.role}
              onChange={(e) => setCreateForm((f) => ({ ...f, role: e.target.value as UserForm["role"] }))}
            >
              <option value="member">member</option>
              <option value="admin">admin</option>
            </select>
          </div>
          {createError && <p className="result error">{createError}</p>}
          <button onClick={createUser} disabled={creating || !createForm.name || !createForm.email}>
            {creating ? "Creating…" : "Create"}
          </button>
        </section>

        {/* List */}
        <section className="card">
          <div className="stream-header">
            <h2>Users {total > 0 && <span className="stream-meta">({total} total)</span>}</h2>
            <button onClick={load} disabled={loading} style={{ padding: "0.25rem 0.6rem", fontSize: "0.8rem" }}>
              {loading ? "…" : "Refresh"}
            </button>
          </div>

          {error && <p className="result error">{error}</p>}

          {!loading && users.length === 0 && (
            <p className="hint" style={{ marginTop: "0.75rem" }}>No users yet — create one above.</p>
          )}

          <ul className="user-list">
            {users.map((u) => (
              <li
                key={u.id}
                className={`user-row${selected?.id === u.id ? " user-row--selected" : ""}`}
                onClick={() => selectUser(u)}
              >
                <div className="user-avatar">{u.name[0]?.toUpperCase()}</div>
                <div className="user-info">
                  <span className="user-name">{u.name}</span>
                  <span className="user-email">{u.email}</span>
                </div>
                <span className={`badge badge--${u.role === "admin" ? "purple" : "gray"}`}>{u.role}</span>
              </li>
            ))}
          </ul>
        </section>

        {/* Edit panel */}
        {selected && (
          <section className="card" style={{ gridColumn: "1 / -1" }}>
            <div className="stream-header">
              <h2>Edit user #{selected.id}</h2>
              <button onClick={() => setSelected(null)} style={{ background: "none", border: "1px solid var(--border)", color: "var(--muted)", padding: "0.2rem 0.5rem", fontSize: "0.8rem" }}>
                ✕ Close
              </button>
            </div>
            <div className="users-edit-row">
              <div className="field-group">
                <label className="field-label">Name</label>
                <input value={editForm.name} onChange={(e) => setEditForm((f) => ({ ...f, name: e.target.value }))} />
              </div>
              <div className="field-group">
                <label className="field-label">Email</label>
                <input type="email" value={editForm.email} onChange={(e) => setEditForm((f) => ({ ...f, email: e.target.value }))} />
              </div>
              <div className="field-group">
                <label className="field-label">Role</label>
                <select value={editForm.role} onChange={(e) => setEditForm((f) => ({ ...f, role: e.target.value as UserForm["role"] }))}>
                  <option value="member">member</option>
                  <option value="admin">admin</option>
                </select>
              </div>
            </div>
            <div className="row" style={{ marginTop: "1rem" }}>
              <button onClick={saveUser} disabled={saving}>{saving ? "Saving…" : "Save changes"}</button>
              <button onClick={deleteUser} disabled={deleting} className="btn-danger">
                {deleting ? "Deleting…" : "Delete user"}
              </button>
            </div>
          </section>
        )}
      </div>
    </div>
  );
}
