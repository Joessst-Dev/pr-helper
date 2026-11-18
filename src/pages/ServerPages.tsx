import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FolderOpen, Plus, Trash2 } from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api, errorMessage } from "../lib/api";
import type { ProviderKind } from "../lib/bindings/ProviderKind";
import type { Server } from "../lib/bindings/Server";
import type { SettingsOverrides } from "../lib/bindings/SettingsOverrides";
import type { Skill } from "../lib/bindings/Skill";
import { EMPTY_OVERRIDES } from "../lib/settings";
import { SettingsForm } from "../components/SettingsForm";
import { Badge, Button, Card, ErrorBanner, Field, Input, PageHeader, Segmented, Select, Spinner, TextArea } from "../components/ui";

const URL_HINT: Record<ProviderKind, string> = {
  github: "https://github.com, or your GitHub Enterprise URL (https://ghe.example.com)",
  gitlab: "https://gitlab.com, or your self-managed GitLab URL",
};

const TOKEN_HINT: Record<ProviderKind, string> = {
  github: "Personal access token with repo and read:org scopes (fine-grained: pull requests read & write, contents read).",
  gitlab: "Personal access token with the api scope.",
};

const SCOPE_HINT: Record<ProviderKind, string> = {
  github: "Search qualifiers limiting \"All visible\", e.g. org:acme repo:acme/app. Empty = your account and all your orgs.",
  gitlab: "Comma separated group paths limiting \"All visible\", e.g. platform, team/backend. Empty = everything you can see.",
};

function ConnectionForm({ server, onSaved }: { server?: Server; onSaved: (s: Server) => void }) {
  const qc = useQueryClient();
  const [kind, setKind] = useState<ProviderKind>(server?.kind ?? "github");
  const [name, setName] = useState(server?.name ?? "");
  const [baseUrl, setBaseUrl] = useState(server?.baseUrl ?? "");
  const [token, setToken] = useState("");
  const [scope, setScope] = useState(server?.prScope ?? "");

  const save = useMutation({
    mutationFn: () =>
      api.saveServer(server?.id ?? null, {
        kind,
        name,
        baseUrl: baseUrl || (kind === "github" ? "https://github.com" : "https://gitlab.com"),
        prScope: scope || null,
        token: token || null,
      }),
    onSuccess: (s) => {
      qc.invalidateQueries({ queryKey: ["servers"] });
      setToken("");
      onSaved(s);
    },
  });

  return (
    <form
      className="max-w-xl space-y-4"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <Field label="Provider">
        <Segmented
          value={kind}
          onChange={setKind}
          options={[
            { value: "github", label: "GitHub" },
            { value: "gitlab", label: "GitLab" },
          ]}
        />
      </Field>
      <Field label="Name">
        <Input required value={name} onChange={(e) => setName(e.target.value)} placeholder="Work GitLab" />
      </Field>
      <Field label="Server URL" hint={URL_HINT[kind]}>
        <Input
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
          placeholder={kind === "github" ? "https://github.com" : "https://gitlab.com"}
        />
      </Field>
      <Field label="Access token" hint={`${TOKEN_HINT[kind]} Stored in your system keychain.`}>
        <Input
          type="password"
          autoComplete="off"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder={server ? "Leave empty to keep the stored token" : ""}
          required={!server}
        />
      </Field>
      <Field label="Scope (optional)" hint={SCOPE_HINT[kind]}>
        <Input value={scope} onChange={(e) => setScope(e.target.value)} />
      </Field>
      {save.error && <ErrorBanner error={errorMessage(save.error)} />}
      <div className="flex items-center gap-3">
        <Button type="submit" variant="primary" busy={save.isPending}>
          {server ? "Save connection" : "Connect"}
        </Button>
        {server?.username && <span className="text-xs text-muted">Connected as {server.username}</span>}
      </div>
    </form>
  );
}

export function ServerFormPage() {
  const navigate = useNavigate();
  return (
    <div>
      <PageHeader title="Add server" subtitle="Connect a GitHub or GitLab account. The token is verified before saving." />
      <div className="px-6 py-5">
        <ConnectionForm onSaved={(s) => navigate(`/servers/${s.id}`)} />
      </div>
    </div>
  );
}

type Tab = "connection" | "review" | "repos" | "skills";

export function ServerSettingsPage() {
  const { serverId = "" } = useParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [tab, setTab] = useState<Tab>("review");
  const servers = useQuery({ queryKey: ["servers"], queryFn: api.listServers });
  const server = servers.data?.find((s) => s.id === serverId);

  const remove = useMutation({
    mutationFn: () => api.deleteServer(serverId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["servers"] });
      navigate("/");
    },
  });

  if (servers.isLoading) return <Spinner className="m-8" />;
  if (!server) return <ErrorBanner error="Server not found" />;

  return (
    <div>
      <PageHeader
        title={`${server.name} settings`}
        subtitle={server.baseUrl}
        actions={
          <Button
            variant="danger"
            busy={remove.isPending}
            onClick={() => {
              if (confirm(`Remove ${server.name}? Its review history and skills are deleted too.`)) remove.mutate();
            }}
          >
            <Trash2 className="size-3.5" /> Remove server
          </Button>
        }
      />
      <div className="px-6 py-4">
        <Segmented
          value={tab}
          onChange={setTab}
          options={[
            { value: "review", label: "Review settings" },
            { value: "repos", label: "Repository overrides" },
            { value: "skills", label: "Skills" },
            { value: "connection", label: "Connection" },
          ]}
        />
      </div>
      <div className="px-6 pb-10">
        {tab === "connection" && <ConnectionForm server={server} onSaved={() => {}} />}
        {tab === "review" && <ServerReviewSettings server={server} />}
        {tab === "repos" && <RepoOverrides serverId={server.id} />}
        {tab === "skills" && <SkillsEditor serverId={server.id} />}
      </div>
    </div>
  );
}

function SaveBar({ dirty, busy, error, onSave }: { dirty: boolean; busy: boolean; error: unknown; onSave: () => void }) {
  return (
    <div className="mt-4 flex items-center gap-3">
      <Button variant="primary" disabled={!dirty} busy={busy} onClick={onSave}>
        Save
      </Button>
      {error ? <span className="text-xs text-danger">{errorMessage(error)}</span> : null}
    </div>
  );
}

function ServerReviewSettings({ server }: { server: Server }) {
  const qc = useQueryClient();
  const [value, setValue] = useState<SettingsOverrides>(server.settings);
  useEffect(() => setValue(server.settings), [server.settings]);
  const inherited = useQuery({
    queryKey: ["inherited", server.id, null],
    queryFn: () => api.inheritedSettings(server.id, null),
  });
  const save = useMutation({
    mutationFn: () => api.saveServerSettings(server.id, value),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["servers"] }),
  });
  return (
    <Card className="max-w-3xl px-5 py-2">
      <SettingsForm value={value} inherited={inherited.data} onChange={setValue} />
      <SaveBar
        dirty={JSON.stringify(value) !== JSON.stringify(server.settings)}
        busy={save.isPending}
        error={save.error}
        onSave={() => save.mutate()}
      />
      <div className="h-3" />
    </Card>
  );
}

function RepoOverrides({ serverId }: { serverId: string }) {
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["repo-settings", serverId], queryFn: () => api.listRepoSettings(serverId) });
  const inherited = useQuery({
    queryKey: ["inherited", serverId, "repo"],
    queryFn: () => api.inheritedSettings(serverId, "*"),
  });
  const [editing, setEditing] = useState<string | null>(null);
  const [newRepo, setNewRepo] = useState("");
  const [value, setValue] = useState<SettingsOverrides>(EMPTY_OVERRIDES);

  const save = useMutation({
    mutationFn: () => api.saveRepoSettings(serverId, editing!, value),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["repo-settings", serverId] });
      setEditing(null);
    },
  });

  const edit = (repo: string, settings: SettingsOverrides) => {
    setEditing(repo);
    setValue(settings);
  };

  return (
    <div className="max-w-3xl space-y-4">
      <p className="text-xs text-muted">
        Repository overrides win over the server settings. Use the full path, e.g. acme/app or group/sub/project.
      </p>
      <div className="flex gap-2">
        <Input placeholder="owner/repository" value={newRepo} onChange={(e) => setNewRepo(e.target.value)} />
        <Button
          disabled={!newRepo.trim()}
          onClick={() => {
            edit(newRepo.trim(), list.data?.find((r) => r.repo === newRepo.trim())?.settings ?? EMPTY_OVERRIDES);
            setNewRepo("");
          }}
        >
          <Plus className="size-3.5" /> Add override
        </Button>
      </div>

      {list.data?.map((r) => (
        <Card key={r.repo} className="flex items-center justify-between px-4 py-2.5">
          <span className="font-mono text-[13px]">{r.repo}</span>
          <div className="flex gap-2">
            <Button size="sm" onClick={() => edit(r.repo, r.settings)}>
              Edit
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={() =>
                api
                  .saveRepoSettings(serverId, r.repo, EMPTY_OVERRIDES)
                  .then(() => qc.invalidateQueries({ queryKey: ["repo-settings", serverId] }))
              }
            >
              <Trash2 className="size-3.5" />
            </Button>
          </div>
        </Card>
      ))}

      {editing && (
        <Card className="px-5 py-3">
          <div className="mb-1 font-mono text-[13px] font-medium">{editing}</div>
          <SettingsForm value={value} inherited={inherited.data} onChange={setValue} />
          <div className="flex gap-2">
            <SaveBar dirty busy={save.isPending} error={save.error} onSave={() => save.mutate()} />
            <Button variant="ghost" className="mt-4" onClick={() => setEditing(null)}>
              Cancel
            </Button>
          </div>
        </Card>
      )}
    </div>
  );
}

const SKILL_TEMPLATE = `When reviewing, check that:
-
`;

function SkillsEditor({ serverId }: { serverId: string }) {
  const qc = useQueryClient();
  const skills = useQuery({ queryKey: ["skills", serverId], queryFn: () => api.listSkills(serverId) });
  const [draft, setDraft] = useState<{ id: string | null; repo: string; name: string; description: string; path: string; content: string; enabled: boolean } | null>(null);

  const save = useMutation({
    mutationFn: () =>
      api.saveSkill(draft!.id, {
        serverId,
        repo: draft!.repo.trim() || null,
        name: draft!.name,
        description: draft!.description,
        path: draft!.path.trim() || null,
        content: draft!.path.trim() ? null : draft!.content,
        enabled: draft!.enabled,
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["skills", serverId] });
      setDraft(null);
    },
  });

  const toggle = (s: Skill) =>
    api
      .saveSkill(s.id, { ...s, enabled: !s.enabled })
      .then(() => qc.invalidateQueries({ queryKey: ["skills", serverId] }));

  const pickFolder = async () => {
    const dir = await openDialog({ directory: true, title: "Choose a folder containing SKILL.md" });
    if (typeof dir === "string" && draft) {
      const name = dir.split(/[\\/]/).filter(Boolean).pop() ?? "";
      setDraft({ ...draft, path: dir, name: draft.name || name });
    }
  };

  return (
    <div className="max-w-3xl space-y-4">
      <p className="text-xs text-muted">
        Skills are handed to the reviewer as a plugin. Server-wide skills apply to every repository; repository
        skills only to that one.
      </p>
      <Button
        onClick={() =>
          setDraft({ id: null, repo: "", name: "", description: "", path: "", content: SKILL_TEMPLATE, enabled: true })
        }
      >
        <Plus className="size-3.5" /> Add skill
      </Button>

      {skills.data?.map((s) => (
        <Card key={s.id} className="flex items-center gap-3 px-4 py-2.5">
          <input
            type="checkbox"
            aria-label={`Enable ${s.name}`}
            className="accent-[var(--accent)]"
            checked={s.enabled}
            onChange={() => toggle(s)}
          />
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span className="font-medium">{s.name}</span>
              <Badge tone={s.repo ? "info" : "neutral"}>{s.repo ?? "all repositories"}</Badge>
              <Badge>{s.path ? "folder" : "inline"}</Badge>
            </div>
            <div className="truncate text-xs text-muted">{s.path ?? s.description}</div>
          </div>
          <Button
            size="sm"
            onClick={() =>
              setDraft({
                id: s.id,
                repo: s.repo ?? "",
                name: s.name,
                description: s.description,
                path: s.path ?? "",
                content: s.content ?? "",
                enabled: s.enabled,
              })
            }
          >
            Edit
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => api.deleteSkill(s.id).then(() => qc.invalidateQueries({ queryKey: ["skills", serverId] }))}
          >
            <Trash2 className="size-3.5" />
          </Button>
        </Card>
      ))}

      {draft && (
        <Card className="space-y-3 p-5">
          <div className="grid grid-cols-2 gap-3">
            <Field label="Name">
              <Input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} />
            </Field>
            <Field label="Repository" hint="Empty = all repositories on this server">
              <Input
                placeholder="owner/repository"
                value={draft.repo}
                onChange={(e) => setDraft({ ...draft, repo: e.target.value })}
              />
            </Field>
          </div>
          <Field label="When to use" hint="Tells the reviewer when this skill is relevant.">
            <Input value={draft.description} onChange={(e) => setDraft({ ...draft, description: e.target.value })} />
          </Field>
          <Field label="Source">
            <Select
              value={draft.path ? "folder" : "inline"}
              onChange={(e) => (e.target.value === "folder" ? pickFolder() : setDraft({ ...draft, path: "" }))}
            >
              <option value="inline">Write it here</option>
              <option value="folder">Use a skill folder (SKILL.md)</option>
            </Select>
          </Field>
          {draft.path ? (
            <div className="flex items-center gap-2">
              <code className="flex-1 truncate rounded bg-bg px-2 py-1.5 text-xs">{draft.path}</code>
              <Button size="sm" onClick={pickFolder}>
                <FolderOpen className="size-3.5" /> Change
              </Button>
            </div>
          ) : (
            <Field label="SKILL.md" hint="Markdown instructions. Frontmatter is generated from the name and description if you omit it.">
              <TextArea
                rows={10}
                className="font-mono text-[12px]"
                value={draft.content}
                onChange={(e) => setDraft({ ...draft, content: e.target.value })}
              />
            </Field>
          )}
          {save.error && <ErrorBanner error={errorMessage(save.error)} />}
          <div className="flex gap-2">
            <Button variant="primary" busy={save.isPending} onClick={() => save.mutate()}>
              Save skill
            </Button>
            <Button variant="ghost" onClick={() => setDraft(null)}>
              Cancel
            </Button>
          </div>
        </Card>
      )}
    </div>
  );
}
