import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useQueryClient } from "@tanstack/react-query";
import type { ProgressEvent } from "./bindings/ProgressEvent";
import type { Review } from "./bindings/Review";

/** Keeps the query cache in sync with backend events. Mount once at the app root. */
export function useBackendEvents() {
  const qc = useQueryClient();
  useEffect(() => {
    const unlisteners = [
      listen<Review>("review-updated", (e) => {
        qc.invalidateQueries({ queryKey: ["review", e.payload.id] });
        qc.invalidateQueries({ queryKey: ["reviews"] });
        qc.invalidateQueries({ queryKey: ["prs"] });
      }),
      listen("tracked-updated", () => {
        qc.invalidateQueries({ queryKey: ["tracked"] });
        qc.invalidateQueries({ queryKey: ["prs"] });
      }),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((off) => off()));
    };
  }, [qc]);
}

/** Live progress lines for one review. */
export function useReviewProgress(reviewId: string | undefined, onEvent: (e: ProgressEvent) => void) {
  useEffect(() => {
    if (!reviewId) return;
    const off = listen<ProgressEvent>("review-progress", (e) => {
      if (e.payload.reviewId === reviewId) onEvent(e.payload);
    });
    return () => {
      off.then((f) => f());
    };
  }, [reviewId, onEvent]);
}
