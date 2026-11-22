import { invoke } from "@tauri-apps/api/core";
import type { AppConfig } from "./bindings/AppConfig";
import type { CommentCheck } from "./bindings/CommentCheck";
import type { CriticalFileContent } from "./bindings/CriticalFileContent";
import type { EffectiveSettings } from "./bindings/EffectiveSettings";
import type { EnvironmentStatus } from "./bindings/EnvironmentStatus";
import type { Finding } from "./bindings/Finding";
import type { PostOutcome } from "./bindings/PostOutcome";
import type { PostRequest } from "./bindings/PostRequest";
import type { PrDetail } from "./bindings/PrDetail";
import type { PrFilter } from "./bindings/PrFilter";
import type { PrListItem } from "./bindings/PrListItem";
import type { RepoSettingsEntry } from "./bindings/RepoSettingsEntry";
import type { Review } from "./bindings/Review";
import type { ReviewDetail } from "./bindings/ReviewDetail";
import type { Server } from "./bindings/Server";
import type { ServerInput } from "./bindings/ServerInput";
import type { SettingsOverrides } from "./bindings/SettingsOverrides";
import type { Skill } from "./bindings/Skill";
import type { SkillInput } from "./bindings/SkillInput";
import type { TicketLoad } from "./bindings/TicketLoad";
import type { TicketSource } from "./bindings/TicketSource";
import type { TicketSourceInput } from "./bindings/TicketSourceInput";
import type { TrackedPr } from "./bindings/TrackedPr";

/** Typed wrappers around the Tauri commands in `src-tauri/src/commands.rs`. */
export const api = {
  listServers: () => invoke<Server[]>("list_servers"),
  saveServer: (id: string | null, input: ServerInput) => invoke<Server>("save_server", { id, input }),
  deleteServer: (id: string) => invoke<void>("delete_server", { id }),
  saveServerSettings: (id: string, settings: SettingsOverrides) =>
    invoke<void>("save_server_settings", { id, settings }),

  listTicketSources: () => invoke<TicketSource[]>("list_ticket_sources"),
  saveTicketSource: (id: string | null, input: TicketSourceInput) =>
    invoke<TicketSource>("save_ticket_source", { id, input }),
  deleteTicketSource: (id: string) => invoke<void>("delete_ticket_source", { id }),
  testTicketSource: (id: string, key: string) => invoke<string>("test_ticket_source", { id, key }),

  listPrs: (serverId: string, filter: PrFilter) => invoke<PrListItem[]>("list_prs", { serverId, filter }),
  getPrDetail: (serverId: string, projectId: string, number: number) =>
    invoke<PrDetail>("get_pr_detail", { serverId, projectId, number }),
  loadPrTickets: (serverId: string, repo: string, keys: string[], requestId: string) =>
    invoke<TicketLoad[]>("load_pr_tickets", { serverId, repo, keys, requestId }),
  loadTicketImage: (serverId: string, repo: string, key: string, attachmentId: string) =>
    invoke<string>("load_ticket_image", { serverId, repo, key, attachmentId }),
  cancelTicketLoad: (requestId: string) => invoke<boolean>("cancel_ticket_load", { requestId }),
  startReview: (serverId: string, projectId: string, number: number) =>
    invoke<Review>("start_review", { serverId, projectId, number }),
  cancelReview: (reviewId: string) => invoke<boolean>("cancel_review", { reviewId }),
  getReview: (reviewId: string) => invoke<ReviewDetail>("get_review", { reviewId }),
  getReviewFile: (reviewId: string, path: string) =>
    invoke<CriticalFileContent>("get_review_file", { reviewId, path }),
  listReviewsForPr: (serverId: string, repo: string, number: number) =>
    invoke<Review[]>("list_reviews_for_pr", { serverId, repo, number }),
  listRecentReviews: (limit: number) => invoke<Review[]>("list_recent_reviews", { limit }),
  updateFinding: (id: string, selected: boolean, proposedComment: string) =>
    invoke<Finding>("update_finding", { id, selected, proposedComment }),
  checkCommentText: (serverId: string, repo: string, text: string) =>
    invoke<CommentCheck>("check_comment_text", { serverId, repo, text }),
  postFindings: (request: PostRequest) => invoke<PostOutcome>("post_findings", { request }),
  resumeCommand: (reviewId: string) => invoke<string>("resume_command", { reviewId }),

  listTracked: () => invoke<TrackedPr[]>("list_tracked"),
  untrackPr: (serverId: string, repo: string, number: number) =>
    invoke<void>("untrack_pr", { serverId, repo, number }),

  getGlobalSettings: () => invoke<SettingsOverrides>("get_global_settings"),
  saveGlobalSettings: (settings: SettingsOverrides) => invoke<void>("save_global_settings", { settings }),
  getRepoSettings: (serverId: string, repo: string) =>
    invoke<SettingsOverrides>("get_repo_settings", { serverId, repo }),
  saveRepoSettings: (serverId: string, repo: string, settings: SettingsOverrides) =>
    invoke<void>("save_repo_settings", { serverId, repo, settings }),
  listRepoSettings: (serverId: string) => invoke<RepoSettingsEntry[]>("list_repo_settings", { serverId }),
  effectiveSettings: (serverId: string, repo: string | null) =>
    invoke<EffectiveSettings>("effective_settings", { serverId, repo }),
  inheritedSettings: (serverId: string | null, repo: string | null) =>
    invoke<EffectiveSettings>("inherited_settings", { serverId, repo }),
  getAppConfig: () => invoke<AppConfig>("get_app_config"),
  saveAppConfig: (config: AppConfig) => invoke<void>("save_app_config", { config }),

  listSkills: (serverId: string) => invoke<Skill[]>("list_skills", { serverId }),
  saveSkill: (id: string | null, input: SkillInput) => invoke<Skill>("save_skill", { id, input }),
  deleteSkill: (id: string) => invoke<void>("delete_skill", { id }),

  checkEnvironment: () => invoke<EnvironmentStatus>("check_environment"),
};

/** Tauri rejects with the serialized `AppError` string. */
export function errorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
