import clsx from "clsx";
import { Bell, GitPullRequest, Plus, Settings, SlidersHorizontal } from "lucide-react";
import { createHashRouter, Navigate, NavLink, Outlet, RouterProvider } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { api } from "./lib/api";
import { useBackendEvents } from "./lib/events";
import { PullRequestsPage } from "./pages/PullRequestsPage";
import { ReviewPage } from "./pages/ReviewPage";
import { ServerFormPage, ServerSettingsPage } from "./pages/ServerPages";
import { SettingsPage } from "./pages/SettingsPage";
import { WatchingPage } from "./pages/WatchingPage";
import { Spinner } from "./components/ui";

function navClass({ isActive }: { isActive: boolean }) {
  return clsx(
    "group flex h-8 items-center gap-2 rounded-md px-2 text-[13px]",
    isActive ? "bg-accent-soft font-medium text-accent" : "text-fg/80 hover:bg-panel-2",
  );
}

function Layout() {
  useBackendEvents();
  const servers = useQuery({ queryKey: ["servers"], queryFn: api.listServers });
  const tracked = useQuery({ queryKey: ["tracked"], queryFn: api.listTracked });
  const newChanges = tracked.data?.filter((t) => t.hasNewChanges).length ?? 0;

  return (
    <div className="flex h-full">
      <aside className="flex w-60 shrink-0 flex-col border-r border-line bg-panel">
        <div className="flex h-12 items-center gap-2 border-b border-line px-4">
          <div className="grid size-6 place-items-center rounded-md bg-accent text-accent-fg">
            <GitPullRequest className="size-3.5" />
          </div>
          <span className="text-sm font-semibold tracking-tight">PR Helper</span>
        </div>

        <nav className="flex-1 overflow-y-auto p-2">
          <div className="px-2 pt-2 pb-1 text-[11px] font-medium tracking-wide text-muted uppercase">Servers</div>
          {servers.isLoading && <Spinner className="mx-2 my-1" />}
          {servers.data?.map((s) => (
            <div key={s.id} className="group relative">
              <NavLink to={`/servers/${s.id}`} end className={navClass}>
                <span className="grid size-5 place-items-center rounded bg-panel-2 text-[10px] font-bold text-muted uppercase">
                  {s.kind === "github" ? "GH" : "GL"}
                </span>
                <span className="truncate pr-6">{s.name}</span>
              </NavLink>
              <NavLink
                to={`/servers/${s.id}/edit`}
                title="Server settings"
                aria-label="Server settings"
                className="absolute top-1.5 right-1.5 hidden rounded p-1 text-muted group-hover:block hover:bg-panel hover:text-fg"
              >
                <SlidersHorizontal className="size-3.5" />
              </NavLink>
            </div>
          ))}
          <NavLink to="/servers/new" className={navClass}>
            <Plus className="size-4 text-muted" />
            <span className="text-muted">Add server</span>
          </NavLink>

          <div className="mt-4 px-2 pt-2 pb-1 text-[11px] font-medium tracking-wide text-muted uppercase">Activity</div>
          <NavLink to="/watching" className={navClass}>
            <Bell className="size-4" />
            <span className="flex-1">Watching</span>
            {newChanges > 0 && (
              <span className="rounded-full bg-warn px-1.5 text-[11px] font-semibold text-white">{newChanges}</span>
            )}
          </NavLink>
          <NavLink to="/settings" className={navClass}>
            <Settings className="size-4" />
            Settings
          </NavLink>
        </nav>
      </aside>

      <main className="min-w-0 flex-1 overflow-y-auto">
        <Outlet />
      </main>
    </div>
  );
}

function Home() {
  const servers = useQuery({ queryKey: ["servers"], queryFn: api.listServers });
  if (servers.isLoading) return null;
  const first = servers.data?.[0];
  return <Navigate to={first ? `/servers/${first.id}` : "/servers/new"} replace />;
}

const router = createHashRouter([
  {
    path: "/",
    element: <Layout />,
    children: [
      { index: true, element: <Home /> },
      { path: "servers/new", element: <ServerFormPage /> },
      { path: "servers/:serverId", element: <PullRequestsPage /> },
      { path: "servers/:serverId/edit", element: <ServerSettingsPage /> },
      { path: "reviews/:reviewId", element: <ReviewPage /> },
      { path: "watching", element: <WatchingPage /> },
      { path: "settings", element: <SettingsPage /> },
    ],
  },
]);

export default function App() {
  return <RouterProvider router={router} />;
}
