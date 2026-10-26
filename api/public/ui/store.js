import { apiClient } from "./api.js";
class StateStore {
  constructor(api = apiClient, openEvents = () => new EventSource("/api/v1/events"), onError = () => {
  }) {
    this.api = api;
    this.openEvents = openEvents;
    this.onError = onError;
  }
  snapshot = null;
  observation = null;
  statuses = {};
  samples = null;
  listeners = /* @__PURE__ */ new Set();
  telemetryListeners = /* @__PURE__ */ new Set();
  connections = /* @__PURE__ */ new Set();
  stream = null;
  retry = null;
  stopped = true;
  generation = 0;
  starting = null;
  queue = Promise.resolve();
  get state() {
    return this.snapshot;
  }
  get observed() {
    return this.observation;
  }
  get status() {
    return this.statuses;
  }
  get telemetry() {
    return this.samples;
  }
  /** Publish a telemetry sample. Only telemetry subscribers hear of it: no desired or observed state changed. */
  setTelemetry(sample) {
    this.samples = sample;
    for (const listener of this.telemetryListeners) listener();
  }
  subscribeTelemetry(listener) {
    this.telemetryListeners.add(listener);
    return () => {
      this.telemetryListeners.delete(listener);
    };
  }
  subscribe(listener) {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }
  subscribeConnection(listener) {
    this.connections.add(listener);
    return () => {
      this.connections.delete(listener);
    };
  }
  start() {
    if (!this.stopped) return this.starting ?? Promise.resolve();
    this.stopped = false;
    this.connect();
    this.starting = this.reload();
    return this.starting;
  }
  stop() {
    this.stopped = true;
    this.generation++;
    this.starting = null;
    this.stream?.close();
    this.stream = null;
    if (this.retry !== null) clearTimeout(this.retry);
    this.retry = null;
  }
  reload() {
    return this.enqueue(async (valid) => {
      const [state, observed, decoder, finder] = await Promise.all([
        this.api.state(),
        this.api.observed(),
        this.api.get("/api/v1/status").catch(() => ({ connected: false, outputs: [] })),
        this.api.get("/api/v1/catalog/ndi_sources").catch(() => this.statuses.finder ?? { list: {} })
      ]);
      if (!valid()) return;
      this.observation = observed;
      this.statuses = { ...this.statuses, decoder, finder };
      this.publish(state);
    });
  }
  /** Re-read the sync observations and pending network change without touching desired state. */
  refreshObserved() {
    return this.enqueue(async (valid) => {
      const observed = await this.api.observed();
      if (valid()) {
        this.observation = observed;
        this.notify([]);
      }
    });
  }
  refreshSources() {
    return this.enqueue(async (valid) => {
      const finder = await this.api.get("/api/v1/catalog/ndi_sources");
      if (valid() && JSON.stringify(finder) !== JSON.stringify(this.statuses.finder)) {
        this.statuses = { ...this.statuses, finder };
        this.notify([]);
      }
    });
  }
  apply(event) {
    return this.enqueue(async (valid) => {
      if ("state_changed" in event) {
        const { revision, domains } = event.state_changed;
        if (this.snapshot && revision <= this.snapshot.revision) return;
        if (!this.snapshot || revision !== this.snapshot.revision + 1) {
          const state = await this.api.state();
          if (valid()) this.publish(state);
          return;
        }
        const unique = [...new Set(domains)];
        const replies = await Promise.all(unique.map((domain) => this.api.domain(domain)));
        if (!unique.length || replies.some((reply) => reply.revision !== revision)) {
          const state = await this.api.state();
          if (valid()) this.publish(state);
          return;
        }
        if (!valid()) return;
        const next = { ...this.snapshot, revision };
        unique.forEach((domain, index) => {
          Object.assign(next, { [domain]: replies[index].state });
        });
        this.publish(next, unique);
      } else if ("observed" in event) {
        if (this.observation) {
          const { domain, delta } = event.observed;
          this.observation = { ...this.observation, domains: { ...this.observation.domains, [domain]: delta } };
        }
        this.notify([]);
      } else if ("status" in event) {
        const { source, data } = event.status;
        const edge = source === "decoder" && typeof data === "object" && data !== null && "type" in data && (data.type === "connection" || data.type === "routing");
        const status = edge ? await this.api.get("/api/v1/status") : data;
        if (!valid()) return;
        this.statuses = { ...this.statuses, [source]: status };
        this.notify([]);
      } else if ("drift" in event || "network_pending" in event) {
        const observed = await this.api.observed();
        if (valid()) {
          this.observation = observed;
          this.notify([]);
        }
      }
    });
  }
  enqueue(work) {
    const generation = this.generation;
    const valid = () => generation === this.generation;
    const next = this.queue.then(() => valid() ? work(valid) : void 0);
    this.queue = next.catch(() => {
    });
    return next;
  }
  publish(state, domains) {
    if (this.snapshot && state.revision < this.snapshot.revision) return;
    this.snapshot = state;
    this.notify(domains ?? Object.keys(state).filter((key) => key !== "revision" && key !== "schema_version"));
  }
  notify(domains) {
    if (this.snapshot) for (const listener of this.listeners) listener(this.snapshot, domains);
  }
  connected(connected) {
    for (const listener of this.connections) listener(connected);
  }
  connect() {
    this.stream?.close();
    if (this.stopped) return;
    const stream = this.openEvents();
    this.stream = stream;
    stream.addEventListener("open", () => {
      if (this.stream === stream && !this.stopped) this.connected(true);
    });
    stream.addEventListener("event", (event) => {
      if (this.stream !== stream || this.stopped) return;
      try {
        void this.apply(JSON.parse(event.data)).catch(this.onError);
      } catch (error) {
        this.onError(error);
      }
    });
    stream.addEventListener("resync", () => {
      if (this.stream === stream && !this.stopped) void this.reload().catch(this.onError);
    });
    stream.onerror = () => {
      if (this.stream !== stream || this.stopped) return;
      stream.close();
      this.stream = null;
      this.connected(false);
      this.retry = setTimeout(() => {
        this.retry = null;
        this.connect();
        if (!this.stopped) void this.reload().catch(this.onError);
      }, 5e3);
    };
  }
}
const store = new StateStore();
export {
  StateStore,
  store
};
