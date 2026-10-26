import { store, type Telemetry } from './store';
import { expose } from './legacy-globals';
import type { OutputStatus } from './legacy-types';

/**
 * What the pages draw: the store's desired state, the decoder and finder status it
 * received and the telemetry samples, combined on demand. It is derived, read-only
 * and never kept between calls, so no page can be left holding an old copy.
 */
export interface LiveView {
  outputs: OutputStatus[];
  sources: string[];
  sourceIPs: Record<string, string>;
  connected: boolean;
  source_name: string;
  platform: string;
  total_frames: number;
  dropped_frames: number;
  audio_enabled: boolean;
  display_width: number;
  display_height: number;
  display_refresh: number;
  display_refresh_hz: number;
  device_name: string;
  cpu_percent: number;
  memory: NonNullable<Telemetry['system']>['memory'];
  temps: Record<string, number>;
  uptime: string;
  decoder: { tally?: string; audio?: string };
  plugins?: unknown[];
}

/** The decoder's own status reply (`/api/v1/status`), before desired state is applied. */
export interface DecoderStatus extends Partial<OutputStatus> {
  platform?: string;
  device_name?: string;
  total_frames?: number;
  dropped_frames?: number;
  outputs?: OutputStatus[];
  width?: number;
  height?: number;
  refresh?: number;
  refresh_hz?: number;
}

export const emptyLiveView = (): LiveView => ({
  outputs: [], sources: [], sourceIPs: {}, connected: false, source_name: '', platform: '',
  total_frames: 0, dropped_frames: 0, audio_enabled: true,
  display_width: 0, display_height: 0, display_refresh: 0, display_refresh_hz: 0,
  device_name: '', cpu_percent: 0, memory: {}, temps: {}, uptime: '', decoder: {},
});

let memo: { key: unknown[]; view: LiveView } | undefined;

export let liveView = function(): LiveView {
  const key = [store.state, store.status.decoder, store.status.finder, store.telemetry];
  if (memo && memo.key.every((part, index) => part === key[index])) return memo.view;
  const state = store.state;
  const decoder = (store.status.decoder ?? {}) as DecoderStatus;
  const finder = (store.status.finder ?? {}) as { list?: Record<string, string> };
  const system = store.telemetry?.system;
  const view: LiveView = {
    ...emptyLiveView(),
    ...(system ? { cpu_percent: system.cpu_percent, memory: system.memory, temps: system.temps, uptime: system.uptime } : {}),
    ...(store.telemetry?.plugins ? { plugins: store.telemetry.plugins } : {}),
  };
  if (state) {
    const observed = decoder.outputs ?? [];
    // Hardware observations own output identity; saved intent can include outputs
    // that are not present on this device. Do not manufacture display tiles.
    view.outputs = observed.map(live => {
      const desired = state.outputs[live.output_index + 1];
      return { ...live, output_index: live.output_index,
        output_alias: desired?.alias || live.output_alias || '',
        source_name: live.source_name ?? desired?.source?.name ?? '',
        scale_mode: desired?.video.scale_mode ?? live.scale_mode ?? 'letterbox',
        rotation: desired?.video.rotation ?? live.rotation ?? 0,
      } as OutputStatus;
    });
    view.sourceIPs = Object.fromEntries(Object.entries(finder.list ?? {}).filter(([name]) => name !== 'None'));
    view.sources = Object.keys(view.sourceIPs);
    view.connected = decoder.connected ?? false;
    view.source_name = decoder.source_name ?? '';
    view.platform = decoder.platform ?? '';
    view.total_frames = decoder.total_frames ?? 0;
    view.dropped_frames = decoder.dropped_frames ?? 0;
    view.audio_enabled = decoder.audio_enabled ?? true;
    view.display_width = decoder.width ?? decoder.display_width ?? 0;
    view.display_height = decoder.height ?? decoder.display_height ?? 0;
    view.display_refresh = decoder.refresh ?? decoder.display_refresh ?? 0;
    view.display_refresh_hz = decoder.refresh_hz ?? decoder.display_refresh_hz ?? 0;
    view.device_name = state.device.ndi_alias || decoder.device_name || '';
    view.decoder = {
      tally: state.ndi.tally === 'on' ? 'TallyOn' : state.ndi.tally === 'video' ? 'TallyVideo' : 'TallyOff',
      audio: state.audio.outputs[1]?.enabled ? 'NDIAudioEn' : 'NDIAudioDis',
    };
  }
  memo = { key, view };
  return view;
};

expose('liveView', () => liveView, value => { liveView = value; });
