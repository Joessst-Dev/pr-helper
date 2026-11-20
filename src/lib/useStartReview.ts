import { useNavigate } from "react-router";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "./api";

/** Starts a review of a PR and opens it. */
export function useStartReview(serverId: string, projectId: string, number: number) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => api.startReview(serverId, projectId, number),
    onSuccess: (review) => {
      qc.invalidateQueries({ queryKey: ["prs", serverId] });
      qc.invalidateQueries({ queryKey: ["pr", serverId, projectId, number] });
      navigate(`/reviews/${review.id}`);
    },
  });
}

/** Route of the in-app PR detail page. GitHub project ids contain a slash. */
export function prDetailPath(serverId: string, projectId: string, number: number) {
  return `/servers/${serverId}/prs/${encodeURIComponent(projectId)}/${number}`;
}
