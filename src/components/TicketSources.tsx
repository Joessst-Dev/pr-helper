import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { api, errorMessage } from "../lib/api";
import type { TicketSource } from "../lib/bindings/TicketSource";
import type { TicketSourceKind } from "../lib/bindings/TicketSourceKind";
import { Badge, Button, Card, ErrorBanner, Field, Input, Segmented } from "./ui";

export const TICKET_KIND_LABEL: Record<TicketSourceKind, string> = {
  jira_cloud: "Jira Cloud",
  jira_server: "Jira Server / Data Center",
  mcp: "MCP server",
};

const DEFAULT_MCP_TOOLS = "getJiraIssue, getAccessibleAtlassianResources";

export function useTicketSources() {
  return useQuery({ queryKey: ["ticket-sources"], queryFn: api.listTicketSources });
}

type Draft = {
  id: string | null;
  kind: TicketSourceKind;
  name: string;
  baseUrl: string;
  email: string;
  token: string;
  mcpServer: string;
  mcpTools: string;
  model: string;
  extraFields: string;
};

function toDraft(s?: TicketSource): Draft {
  return {
    id: s?.id ?? null,
    kind: s?.kind ?? "jira_cloud",
    name: s?.name ?? "",
    baseUrl: s?.baseUrl ?? "",
    email: s?.email ?? "",
    token: "",
    mcpServer: s?.mcpServer ?? "",
    mcpTools: s?.mcpTools ?? "",
    model: s?.model ?? "",
    extraFields: s?.extraFields ?? "",
  };
}

const orNull = (v: string) => (v.trim() ? v.trim() : null);

export function TicketSourceForm({
  source,
  onDone,
}: {
  source?: TicketSource;
  onDone: () => void;
}) {
  const qc = useQueryClient();
  const [d, setD] = useState<Draft>(() => toDraft(source));
  const set = (patch: Partial<Draft>) => setD((prev) => ({ ...prev, ...patch }));
  const isMcp = d.kind === "mcp";
  // Without a new token the stored one is reused, which the backend only allows for the same URL and type.
  const tokenRequired = !isMcp && (!source || source.kind !== d.kind || source.baseUrl !== d.baseUrl.trim().replace(/\/+$/, ""));

  const save = useMutation({
    mutationFn: () =>
      api.saveTicketSource(d.id, {
        kind: d.kind,
        name: d.name,
        baseUrl: d.baseUrl,
        email: d.kind === "jira_cloud" ? orNull(d.email) : null,
        mcpServer: isMcp ? orNull(d.mcpServer) : null,
        mcpTools: isMcp ? orNull(d.mcpTools) : null,
        model: isMcp ? orNull(d.model) : null,
        extraFields: orNull(d.extraFields),
        token: isMcp ? null : orNull(d.token),
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["ticket-sources"] });
      onDone();
    },
  });

  return (
    <form
      className="space-y-4"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <Field label="Type">
        <Segmented
          value={d.kind}
          onChange={(kind) => set({ kind })}
          options={[
            { value: "jira_cloud", label: "Jira Cloud" },
            { value: "jira_server", label: "Jira Server / DC" },
            { value: "mcp", label: "MCP server" },
          ]}
        />
      </Field>
      <div className="grid grid-cols-2 gap-4">
        <Field label="Name">
          <Input required value={d.name} onChange={(e) => set({ name: e.target.value })} placeholder="Acme Jira" />
        </Field>
        <Field label="Jira URL" hint={isMcp ? "Optional. Used for ticket links and as a hint for the MCP server." : undefined}>
          <Input
            required={!isMcp}
            value={d.baseUrl}
            onChange={(e) => set({ baseUrl: e.target.value })}
            placeholder={d.kind === "jira_server" ? "https://jira.example.com" : "https://acme.atlassian.net"}
          />
        </Field>
        {d.kind === "jira_cloud" && (
          <Field label="Account email">
            <Input required type="email" value={d.email} onChange={(e) => set({ email: e.target.value })} />
          </Field>
        )}
        {!isMcp && (
          <Field
            label={d.kind === "jira_cloud" ? "API token" : "Personal access token"}
            hint="Read access is enough. Stored in your system keychain."
          >
            <Input
              type="password"
              autoComplete="off"
              required={tokenRequired}
              value={d.token}
              onChange={(e) => set({ token: e.target.value })}
              placeholder={tokenRequired ? "" : "Leave empty to keep the stored token"}
            />
          </Field>
        )}
        {isMcp && (
          <>
            <Field label="MCP server name" hint="As configured in Claude Code (claude mcp list), e.g. atlassian.">
              <Input required value={d.mcpServer} onChange={(e) => set({ mcpServer: e.target.value })} />
            </Field>
            <Field label="Allowed tools" hint="The only tools the fetcher may call.">
              <Input
                value={d.mcpTools}
                placeholder={DEFAULT_MCP_TOOLS}
                onChange={(e) => set({ mcpTools: e.target.value })}
              />
            </Field>
            <Field label="Model" hint="For the short fetch run. Empty = haiku.">
              <Input value={d.model} placeholder="haiku" onChange={(e) => set({ model: e.target.value })} />
            </Field>
          </>
        )}
        <Field label="Extra fields (optional)" hint="Field ids to include, e.g. customfield_10035 for acceptance criteria.">
          <Input value={d.extraFields} onChange={(e) => set({ extraFields: e.target.value })} />
        </Field>
      </div>
      {isMcp && (
        <p className="text-xs text-muted">
          Tickets are fetched in a separate Claude run that may only call these MCP tools and never sees the pull request.
          The reviewer itself gets no MCP access.
        </p>
      )}
      {save.error && <ErrorBanner error={errorMessage(save.error)} />}
      <div className="flex gap-2">
        <Button type="submit" variant="primary" busy={save.isPending}>
          {isMcp ? "Save" : "Verify and save"}
        </Button>
        <Button type="button" variant="ghost" onClick={onDone}>
          Cancel
        </Button>
      </div>
    </form>
  );
}

function TestTicket({ source }: { source: TicketSource }) {
  const [key, setKey] = useState("");
  const test = useMutation({ mutationFn: () => api.testTicketSource(source.id, key) });
  return (
    <div className="space-y-2">
      <form
        className="flex gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          test.mutate();
        }}
      >
        <Input
          aria-label={`Ticket key to test ${source.name}`}
          placeholder="ABC-123"
          value={key}
          onChange={(e) => setKey(e.target.value)}
        />
        <Button type="submit" size="sm" busy={test.isPending} disabled={!key.trim()}>
          Test
        </Button>
      </form>
      {test.error && <ErrorBanner error={errorMessage(test.error)} />}
      {test.data && (
        <pre className="max-h-80 overflow-auto whitespace-pre-wrap rounded border border-line bg-panel-2 p-3 font-mono text-xs">
          {test.data}
        </pre>
      )}
    </div>
  );
}

export function TicketSources() {
  const qc = useQueryClient();
  const sources = useTicketSources();
  const [editing, setEditing] = useState<TicketSource | "new" | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const remove = useMutation({
    mutationFn: (id: string) => api.deleteTicketSource(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["ticket-sources"] }),
  });

  return (
    <div className="space-y-2">
      {sources.error && <ErrorBanner error={errorMessage(sources.error)} />}
      {remove.error && <ErrorBanner error={errorMessage(remove.error)} />}
      {sources.data?.length === 0 && !editing && (
        <p className="text-xs text-muted">No ticket sources yet. Add one, then pick it in the review settings below.</p>
      )}
      {sources.data?.map((s) =>
        editing !== "new" && editing?.id === s.id ? (
          <Card key={s.id} className="p-4">
            <TicketSourceForm source={s} onDone={() => setEditing(null)} />
          </Card>
        ) : (
          <Card key={s.id} className="space-y-2 px-4 py-2.5">
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <div className="flex items-center gap-2 text-[13px] font-medium">
                  {s.name} <Badge>{TICKET_KIND_LABEL[s.kind]}</Badge>
                </div>
                <div className="truncate text-xs text-muted">
                  {s.kind === "mcp" ? `MCP server ${s.mcpServer ?? ""}` : s.baseUrl}
                  {s.username && ` · connected as ${s.username}`}
                </div>
              </div>
              <div className="flex shrink-0 gap-2">
                <Button size="sm" variant="ghost" onClick={() => setTesting(testing === s.id ? null : s.id)}>
                  Test
                </Button>
                <Button size="sm" onClick={() => setEditing(s)}>
                  Edit
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`Delete ${s.name}`}
                  title="Delete"
                  busy={remove.isPending && remove.variables === s.id}
                  onClick={() => {
                    if (confirm(`Delete the ticket source ${s.name}?`)) remove.mutate(s.id);
                  }}
                >
                  <Trash2 className="size-3.5" />
                </Button>
              </div>
            </div>
            {testing === s.id && <TestTicket source={s} />}
          </Card>
        ),
      )}
      {editing === "new" ? (
        <Card className="p-4">
          <TicketSourceForm onDone={() => setEditing(null)} />
        </Card>
      ) : (
        <Button size="sm" onClick={() => setEditing("new")}>
          <Plus className="size-3.5" /> Add ticket source
        </Button>
      )}
    </div>
  );
}
