import { Ticket } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { TicketRef } from "../lib/bindings/TicketRef";

/** The tickets that were given to the reviewer as context. */
export function TicketChips({ tickets }: { tickets: TicketRef[] }) {
  if (tickets.length === 0) return null;
  return (
    <span className="flex flex-wrap items-center gap-1">
      {tickets.map((t) =>
        t.url ? (
          <button
            key={t.key}
            type="button"
            title={`Open ${t.key}`}
            className="inline-flex items-center gap-1 rounded border border-line px-1.5 py-0.5 font-mono text-[11px] hover:bg-panel-2"
            onClick={() => openUrl(t.url!)}
          >
            <Ticket className="size-3" /> {t.key}
          </button>
        ) : (
          <span
            key={t.key}
            className="inline-flex items-center gap-1 rounded border border-line px-1.5 py-0.5 font-mono text-[11px]"
          >
            <Ticket className="size-3" /> {t.key}
          </span>
        ),
      )}
    </span>
  );
}
