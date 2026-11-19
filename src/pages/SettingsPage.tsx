import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CircleCheck, CircleX, RefreshCw } from "lucide-react";
import { api } from "../lib/api";
import type { AppConfig } from "../lib/bindings/AppConfig";
import type { SettingsOverrides } from "../lib/bindings/SettingsOverrides";
import type { ToolStatus } from "../lib/bindings/ToolStatus";
import { EMPTY_OVERRIDES } from "../lib/settings";
import { SettingsForm } from "../components/SettingsForm";
import { Button, Card, ErrorBanner, Field, Input, PageHeader, Spinner } from "../components/ui";

function ToolRow({ name, status }: { name: string; status: ToolStatus }) {
  const ok = !status.error && status.version;
  return (
    <div className="flex items-start gap-2 py-1.5">
      {ok ? <CircleCheck className="mt-0.5 size-4 text-ok" /> : <CircleX className="mt-0.5 size-4 text-danger" />}
      <div className="min-w-0">
        <div className="text-[13px] font-medium">
          {name} {status.version && <span className="font-normal text-muted">{status.version}</span>}
        </div>
        <div className="truncate font-mono text-xs text-muted">{status.path ?? "not found"}</div>
        {status.error && <div className="text-xs text-danger">{status.error}</div>}
      </div>
    </div>
  );
}

export function SettingsPage() {
  const qc = useQueryClient();
  const env = useQuery({ queryKey: ["env"], queryFn: api.checkEnvironment });
  const config = useQuery({ queryKey: ["app-config"], queryFn: api.getAppConfig });
  const global = useQuery({ queryKey: ["global-settings"], queryFn: api.getGlobalSettings });
  const defaults = useQuery({ queryKey: ["inherited", null, null], queryFn: () => api.inheritedSettings(null, null) });

  const [cfg, setCfg] = useState<AppConfig | null>(null);
  const [overrides, setOverrides] = useState<SettingsOverrides>(EMPTY_OVERRIDES);
  useEffect(() => config.data && setCfg(config.data), [config.data]);
  useEffect(() => global.data && setOverrides(global.data), [global.data]);

  const saveCfg = useMutation({
    mutationFn: () => api.saveAppConfig(cfg!),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["app-config"] });
      env.refetch();
    },
  });
  const saveGlobal = useMutation({
    mutationFn: () => api.saveGlobalSettings(overrides),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["global-settings"] });
      // Server and repository forms show these values as "inherited".
      qc.invalidateQueries({ queryKey: ["inherited"] });
    },
  });

  return (
    <div>
      <PageHeader title="Settings" subtitle="App configuration and the default review behaviour for every server." />
      <div className="max-w-3xl space-y-6 px-6 py-5">
        <section>
          <div className="mb-2 flex items-center justify-between">
            <h2 className="text-sm font-semibold">Environment</h2>
            <Button size="sm" variant="ghost" busy={env.isFetching} onClick={() => env.refetch()}>
              <RefreshCw className="size-3" /> Re-check
            </Button>
          </div>
          <Card className="px-4 py-2">
            {env.isLoading && <Spinner />}
            {env.data && (
              <>
                <ToolRow name="Claude Code" status={env.data.claude} />
                <ToolRow name="Git" status={env.data.git} />
              </>
            )}
          </Card>
        </section>

        {cfg && (
          <section>
            <h2 className="mb-2 text-sm font-semibold">Application</h2>
            <Card className="space-y-4 p-4">
              <div className="grid grid-cols-2 gap-4">
                <Field label="Claude binary" hint="Empty = find it on your shell PATH">
                  <Input
                    value={cfg.claudePath ?? ""}
                    placeholder="claude"
                    onChange={(e) => setCfg({ ...cfg, claudePath: e.target.value || null })}
                  />
                </Field>
                <Field label="Git binary" hint="Empty = find it on your shell PATH">
                  <Input
                    value={cfg.gitPath ?? ""}
                    placeholder="git"
                    onChange={(e) => setCfg({ ...cfg, gitPath: e.target.value || null })}
                  />
                </Field>
                <Field label="Parallel reviews" hint="Takes effect after a restart">
                  <Input
                    type="number"
                    min={1}
                    max={8}
                    value={cfg.maxConcurrentReviews}
                    onChange={(e) => setCfg({ ...cfg, maxConcurrentReviews: Number(e.target.value) || 1 })}
                  />
                </Field>
                <Field label="Review timeout (minutes)">
                  <Input
                    type="number"
                    min={1}
                    value={cfg.reviewTimeoutMins}
                    onChange={(e) => setCfg({ ...cfg, reviewTimeoutMins: Number(e.target.value) || 30 })}
                  />
                </Field>
              </div>
              <label className="flex items-center gap-2 text-[13px]">
                <input
                  type="checkbox"
                  className="accent-[var(--accent)]"
                  checked={cfg.launchAtLogin}
                  onChange={(e) => setCfg({ ...cfg, launchAtLogin: e.target.checked })}
                />
                Launch at login (starts hidden in the menu bar)
              </label>
              <label className="flex items-center gap-2 text-[13px]">
                <input
                  type="checkbox"
                  className="accent-[var(--accent)]"
                  checked={cfg.keepWorktrees}
                  onChange={(e) => setCfg({ ...cfg, keepWorktrees: e.target.checked })}
                />
                Keep review checkouts so sessions can be continued in a terminal
              </label>
              {saveCfg.error && <ErrorBanner error={saveCfg.error} />}
              <Button
                variant="primary"
                busy={saveCfg.isPending}
                disabled={JSON.stringify(cfg) === JSON.stringify(config.data)}
                onClick={() => saveCfg.mutate()}
              >
                Save
              </Button>
            </Card>
          </section>
        )}

        <section>
          <h2 className="mb-1 text-sm font-semibold">Review defaults</h2>
          <p className="mb-2 text-xs text-muted">Servers and repositories can override each of these.</p>
          <Card className="px-5 py-2">
            <SettingsForm value={overrides} inherited={defaults.data} onChange={setOverrides} />
            <div className="py-3">
              <Button
                variant="primary"
                busy={saveGlobal.isPending}
                disabled={JSON.stringify(overrides) === JSON.stringify(global.data)}
                onClick={() => saveGlobal.mutate()}
              >
                Save defaults
              </Button>
            </div>
          </Card>
        </section>
      </div>
    </div>
  );
}
