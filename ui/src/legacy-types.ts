export type { AudioInput } from './generated/AudioInput';
export type { AudioDestination } from './generated/AudioDestination';
import type { AudioInput } from './generated/AudioInput';
import type { AudioDestination } from './generated/AudioDestination';
import type { OutputAudio } from './generated/OutputAudio';
import type { Telemetry } from './store';

// Legacy routes flatten desired state and observations. Keep their wire shapes
// until round 2; generated native domain envelopes are not interchangeable.
export interface LegacyResult {
  ok?: boolean;
  error?: string;
  errors?: string[];
  cancelled?: boolean;
  revision?: number;
  impact?: { token: string; lines?: string[] };
  presets?: LegacyPreset[];
  next?: string | null;
  path?: string;
  result?: unknown;
  // Plugin lifecycle replies: the resulting state and the failed step's log.
  state?: string;
  log?: string;
}
export interface OutputStatus {
  output_index: number;
  connected: boolean;
  source_name: string;
  // "discovery_server" while the Discovery Server's route is shown.
  source_origin?: string;
  output_alias: string;
  connector: string;
  fps: number;
  codec: string;
  input_width: number;
  input_height: number;
  display_width: number;
  display_height: number;
  display_refresh: number;
  display_refresh_hz: number;
  width: number;
  height: number;
  refresh: number;
  refresh_hz: number;
  scale_mode: import('./generated/ScaleMode').ScaleMode;
  rotation: number;
  audio_enabled: boolean;
  decode_backend: string;
  hw_decode: boolean;
  decode_saturated: boolean;
  decode_fps: number;
}
// The telemetry sample `/api/status` still supplies (see Telemetry in store.ts);
// plugin fragments also receive its plugin-state banner field.
export interface StatusEvent extends Telemetry {
  plugins_state_error?: string | null;
}
// Plugin progress messages share the legacy event shape.
export interface LegacyEvent extends StatusEvent {
  type?: string; name?: string; step?: string; error?: string;
}
export interface VideoMode {
  width: number; height: number; refresh: number; refresh_hz: number; preferred?: boolean;
}
export interface OutputModesReply {
  modes?: VideoMode[]; current?: Partial<VideoMode>; auto_resolution?: boolean;
  supported_rotations?: number[]; rotation?: number;
}
export interface AudioEndpoint {
  id: string; label: string; ready: boolean; reason?: string; mapping?: string; channels?: number;
}
export interface AudioCatalog extends LegacyResult { inputs: AudioEndpoint[]; destinations: AudioEndpoint[] }
export interface AudioReply extends LegacyResult, Pick<OutputAudio, 'enabled' | 'inputs' | 'destinations'> {
  legacy_matrix?: boolean; warning?: string; native?: OutputAudio;
}
// A draft starts with loading/error only, then acquires the saved bus fields.
export interface AudioDraft extends AudioReply {
  loading?: boolean; loaded?: boolean; dirty?: boolean; busy?: boolean; stale?: boolean;
  error: string;
  inputs: AudioInput[];
  destinations: AudioDestination[];
}
export interface LegacyPreset { name: string; source: string; ip?: string }
export interface LegacyPending {
  token: string; address?: string; deadline?: number; mode?: string; profiles?: boolean;
}
export interface NetworkPreview extends LegacyResult {
  needs_prompt?: boolean; warnings?: string[]; duplicate?: boolean; message?: string;
  present?: boolean; applicable?: boolean;
}
export interface BackupSection {
  key: string; selected?: boolean; label: string; changed?: boolean; summary?: string;
}
export interface InspectReply extends LegacyResult, RestorePlan {
  source_device?: { ndi_alias?: string; hostname?: string };
  app?: { firmware?: string }; sections?: BackupSection[];
  extra_outputs?: number[]; created_at?: string;
}
export interface RestorePlan {
  import_id?: string; chosen?: string[]; revision?: number;
  network?: NetworkPreview;
}
export interface RestoreResult {
  ok?: boolean; error?: string; skipped?: string; warning?: string; pending?: boolean;
  vlans?: { id: number; ok?: boolean; error?: string }[];
}
export interface RestoreReply extends LegacyResult {
  relogin?: boolean;
  results?: Record<string, RestoreResult>; network?: LegacyPending; noop?: boolean;
}
export interface ToastInstance { _config: { delay: number }; show(): void }
export interface Bootstrap {
  Toast: {
    new (element: HTMLElement, options: { delay: number }): ToastInstance;
    getOrCreateInstance(element: HTMLElement, options: { delay: number }): ToastInstance;
  };
  Modal: {
    getOrCreateInstance(element: HTMLElement): { show(): void; hide(): void };
    getInstance(element: HTMLElement): { hide(): void } | null;
  };
}
// Reads that still have no native query or observation: each stays on its current route.
export interface LegacyReads {
  '/api/status': StatusEvent;
  '/v1/AboutMe': { FirmwareVersion?: string; Model?: string; HostName?: string; DeviceName?: string; IPAddresses?: string[]; IPAddress?: string; NDIVersion?: string; BuildDate?: string };
  // The OS clock's synchronization; the configured server is saved intent in the store.
  '/v1/DeviceSettings/ntp': { synchronized?: boolean; active_server?: string };
  '/api/v1/system/vaapi-info': { available?: boolean; driver?: string; decode_profiles?: string[] };
  '/api/v1/system/version': { commit?: string; firmware?: string; update_running?: boolean; update_supported?: boolean; update_message?: string; latest_version?: string; updates_available: number };
  '/api/health': { outputs?: { health: string; stall_count?: number }[] };
  '/api/watchdog-stats': { watchdog_mode?: string; recv_stalls?: number; video_stalls?: number; decoder_stalls?: number; display_freezes?: number; reconnects?: number; service_restarts?: number };
  '/api/services': { services?: { name: string; active: string; sub: string }[] };
  // When the device's timer will next fire; the saved schedule is in the store.
  '/v1/DeviceSettings/reboot-schedule': { next?: string | null };
  '/v1/Backup/snapshots': { snapshots?: { name: string; mtime?: string }[] };
}
