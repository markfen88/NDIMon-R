import { apiClient, type ApiClient } from './api.js';
import type { DesiredState } from './generated/DesiredState';
import type { Domain } from './generated/Domain';
import type { Observed } from './generated/Observed';
import type { StatusSource } from './generated/StatusSource';
import type { Event as ControlEvent } from './generated/Event';

type Listener = (state: Readonly<DesiredState>, domains: readonly Domain[]) => void;
type EventStream = Pick<EventSource, 'addEventListener' | 'close' | 'onerror'>;
type StoreApi = Pick<ApiClient, 'state' | 'domain' | 'observed' | 'get'>;

/**
 * Readings without a native observation yet (CPU, memory, temperatures, uptime and
 * plugin status). `/api/status` is their only source; a poller hands each sample
 * to the store so every view reads one place.
 */
export interface Telemetry {
  system?: {
    cpu_percent: number;
    memory: { percent?: number; used_mb?: number; total_mb?: number };
    temps: Record<string, number>;
    uptime: string;
  };
  plugins?: unknown[];
}

/** One revisioned cache; editors retain their own revision for compare-and-set. */
export class StateStore {
  private snapshot: DesiredState | null = null;
  private observation: Observed | null = null;
  private statuses: Partial<Record<StatusSource, unknown>> = {};
  private samples: Telemetry | null = null;
  private readonly listeners = new Set<Listener>();
  private readonly telemetryListeners = new Set<() => void>();
  private readonly connections = new Set<(connected: boolean) => void>();
  private stream: EventStream | null = null;
  private retry: ReturnType<typeof setTimeout> | null = null;
  private stopped = true;
  private generation = 0;
  private starting: Promise<void> | null = null;
  private queue: Promise<void> = Promise.resolve();

  constructor(
    private readonly api: StoreApi = apiClient,
    private readonly openEvents: () => EventStream = () => new EventSource('/api/v1/events'),
    private readonly onError: (error: unknown) => void = () => {},
  ) {}

  get state(): Readonly<DesiredState> | null { return this.snapshot; }
  get observed(): Readonly<Observed> | null { return this.observation; }
  get status(): Readonly<Partial<Record<StatusSource, unknown>>> { return this.statuses; }
  get telemetry(): Readonly<Telemetry> | null { return this.samples; }

  /** Publish a telemetry sample. Only telemetry subscribers hear of it: no desired or observed state changed. */
  setTelemetry(sample: Telemetry): void {
    this.samples = sample;
    for (const listener of this.telemetryListeners) listener();
  }

  subscribeTelemetry(listener: () => void): () => void {
    this.telemetryListeners.add(listener);
    return () => { this.telemetryListeners.delete(listener); };
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  subscribeConnection(listener: (connected: boolean) => void): () => void {
    this.connections.add(listener);
    return () => { this.connections.delete(listener); };
  }

  start(): Promise<void> {
    if (!this.stopped) return this.starting ?? Promise.resolve();
    this.stopped = false;
    // Subscribe first so changes during the initial read are not lost.
    this.connect();
    this.starting = this.reload();
    return this.starting;
  }

  stop(): void {
    this.stopped = true;
    this.generation++;
    this.starting = null;
    this.stream?.close();
    this.stream = null;
    if (this.retry !== null) clearTimeout(this.retry);
    this.retry = null;
  }

  reload(): Promise<void> {
    return this.enqueue(async valid => {
      const [state, observed, decoder, finder] = await Promise.all([
        this.api.state(), this.api.observed(),
        this.api.get('/api/v1/status').catch(() => ({connected:false,outputs:[]})),
        this.api.get('/api/v1/catalog/ndi_sources').catch(() => this.statuses.finder ?? {list:{}}),
      ]);
      if (!valid()) return;
      this.observation = observed;
      this.statuses = { ...this.statuses, decoder, finder };
      this.publish(state);
    });
  }

  /** Re-read the sync observations and pending network change without touching desired state. */
  refreshObserved(): Promise<void> {
    return this.enqueue(async valid => {
      const observed = await this.api.observed();
      if (valid()) { this.observation = observed; this.notify([]); }
    });
  }

  refreshSources(): Promise<void> {
    return this.enqueue(async valid => {
      const finder = await this.api.get('/api/v1/catalog/ndi_sources');
      if (valid() && JSON.stringify(finder) !== JSON.stringify(this.statuses.finder)) {
        this.statuses = { ...this.statuses, finder }; this.notify([]);
      }
    });
  }

  apply(event: ControlEvent): Promise<void> {
    return this.enqueue(async valid => {
      if ('state_changed' in event) {
        const { revision, domains } = event.state_changed;
        if (this.snapshot && revision <= this.snapshot.revision) return;
        if (!this.snapshot || revision !== this.snapshot.revision + 1) {
          const state = await this.api.state();
          if (valid()) this.publish(state);
          return;
        }
        const unique = [...new Set(domains)];
        const replies = await Promise.all(unique.map(domain => this.api.domain(domain)));
        // Never label a document assembled across concurrent commits as atomic.
        if (!unique.length || replies.some(reply => reply.revision !== revision)) {
          const state = await this.api.state();
          if (valid()) this.publish(state);
          return;
        }
        if (!valid()) return;
        const next = { ...this.snapshot, revision };
        unique.forEach((domain, index) => { Object.assign(next, { [domain]: replies[index].state }); });
        this.publish(next, unique);
      } else if ('observed' in event) {
        if (this.observation) {
          const { domain, delta } = event.observed;
          this.observation = { ...this.observation, domains: { ...this.observation.domains, [domain]: delta } };
        }
        this.notify([]);
      } else if ('status' in event) {
        const { source, data } = event.status;
        // Edge events announce a change; they are not a complete status document.
        const edge = source === 'decoder' && typeof data === 'object' && data !== null &&
          'type' in data && (data.type === 'connection' || data.type === 'routing');
        const status = edge ? await this.api.get('/api/v1/status') : data;
        if (!valid()) return;
        this.statuses = { ...this.statuses, [source]: status };
        this.notify([]);
      } else if ('drift' in event || 'network_pending' in event) {
        const observed = await this.api.observed();
        if (valid()) { this.observation = observed; this.notify([]); }
      }
    });
  }

  private enqueue(work: (valid: () => boolean) => Promise<void>): Promise<void> {
    const generation = this.generation;
    const valid = () => generation === this.generation;
    const next = this.queue.then(() => valid() ? work(valid) : undefined);
    // A failed fetch must not prevent resync or later state changes.
    this.queue = next.catch(() => {});
    return next;
  }

  private publish(state: DesiredState, domains?: readonly Domain[]): void {
    if (this.snapshot && state.revision < this.snapshot.revision) return;
    this.snapshot = state;
    this.notify(domains ?? (Object.keys(state).filter(key => key !== 'revision' && key !== 'schema_version') as Domain[]));
  }

  private notify(domains: readonly Domain[]): void {
    if (this.snapshot) for (const listener of this.listeners) listener(this.snapshot, domains);
  }

  private connected(connected: boolean): void {
    for (const listener of this.connections) listener(connected);
  }

  private connect(): void {
    this.stream?.close();
    if (this.stopped) return;
    const stream = this.openEvents();
    this.stream = stream;
    stream.addEventListener('open', () => {
      if (this.stream === stream && !this.stopped) this.connected(true);
    });
    stream.addEventListener('event', event => {
      if (this.stream !== stream || this.stopped) return;
      try { void this.apply(JSON.parse((event as MessageEvent<string>).data) as ControlEvent).catch(this.onError); }
      catch (error) { this.onError(error); }
    });
    stream.addEventListener('resync', () => {
      if (this.stream === stream && !this.stopped) void this.reload().catch(this.onError);
    });
    // SSE keepalive comments are consumed by EventSource, without a state update.
    stream.onerror = () => {
      if (this.stream !== stream || this.stopped) return;
      stream.close();
      this.stream = null;
      this.connected(false);
      this.retry = setTimeout(() => {
        this.retry = null;
        this.connect();
        if (!this.stopped) void this.reload().catch(this.onError);
      }, 5000);
    };
  }
}

// Importing the shared store performs no I/O.
export const store = new StateStore();
