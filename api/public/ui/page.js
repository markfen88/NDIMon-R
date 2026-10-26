"use strict";
(() => {
  // src/legacy-globals.ts
  function expose(name, get, set) {
    Object.defineProperty(globalThis, name, { configurable: true, enumerable: true, get, set });
  }

  // src/state.ts
  var modes = [];
  var toastInst2;
  document.addEventListener("DOMContentLoaded", async () => {
    toastInst2 = new bootstrap.Toast(document.getElementById("toast"), { delay: 4e3 });
    document.querySelectorAll(".nav-item[data-page]").forEach((a) => {
      a.addEventListener("click", (e) => {
        e.preventDefault();
        showPage(a.dataset.page);
      });
    });
    window.addEventListener("hashchange", () => {
      const name = location.hash.replace("#", "");
      if (PAGES.includes(name)) showPage(name, { fromHash: true });
    });
    const first = location.hash.replace("#", "");
    if (PAGES.includes(first) && first !== "dashboard") showPage(first, { fromHash: true });
    await startApp();
  });
  expose("modes", () => modes, (value) => {
    modes = value;
  });
  expose("toastInst", () => toastInst2, (value) => {
    toastInst2 = value;
  });

  // src/api.ts
  var ApiFailure = class extends Error {
    constructor(status, envelope) {
      super(envelope.message);
      this.status = status;
      this.envelope = envelope;
      this.name = "ApiFailure";
    }
    get kind() {
      const error = this.envelope.error;
      return typeof error === "string" ? error : Object.keys(error)[0];
    }
  };
  var ApiClient = class {
    constructor(fetcher = (input, init) => globalThis.fetch(input, init)) {
      this.fetcher = fetcher;
    }
    csrf = null;
    setCsrf(csrf) {
      this.csrf = csrf;
    }
    get csrfToken() {
      return this.csrf;
    }
    async login(password) {
      const session = await this.request("/api/v1/session", "POST", { password });
      this.csrf = session.csrf;
    }
    async logout() {
      await this.request("/api/v1/session", "DELETE");
      this.csrf = null;
    }
    /** Session status; anyone may ask, only a signed-in session is given its token. */
    async session() {
      const session = await this.request("/api/v1/session", "GET");
      if (session.csrf) this.csrf = session.csrf;
      return session;
    }
    /** The server replaces every session, so the reply carries this one's new token. */
    async changePassword(current_password, new_password) {
      const session = await this.request("/api/v1/session/password", "POST", { current_password, new_password });
      this.csrf = session.csrf;
    }
    state() {
      return this.get("/api/v1/state");
    }
    domain(domain) {
      return this.get(`/api/v1/state/${domain}`);
    }
    observed() {
      return this.get("/api/v1/observed");
    }
    get(path) {
      return this.request(path, "GET");
    }
    async command(request, confirm) {
      let body = { ...request, confirm: false };
      for (let round = 0; ; round++) {
        try {
          return await this.request("/api/v1/commands", "POST", body);
        } catch (error) {
          if (!(error instanceof ApiFailure) || error.status !== 409 || !confirm || round >= 2) throw error;
          const refusal = error.envelope.error;
          if (typeof refusal === "string" || !("confirmation_required" in refusal)) throw error;
          const impact = refusal.confirmation_required.impact;
          if (!await confirm(impact)) throw error;
          body = { ...body, expect_impact: impact.token };
        }
      }
    }
    async request(path, method, body) {
      const headers = new Headers({ Accept: "application/json" });
      if (body !== void 0) headers.set("Content-Type", "application/json");
      if (method !== "GET" && this.csrf) headers.set("X-CSRF-Token", this.csrf);
      const response = await this.fetcher(path, {
        method,
        headers,
        credentials: "same-origin",
        cache: "no-store",
        ...body === void 0 ? {} : { body: JSON.stringify(body) }
      });
      if (!response.ok) {
        const envelope = await response.json();
        throw new ApiFailure(response.status, envelope);
      }
      if (response.status === 204) return void 0;
      return response.json();
    }
  };
  var apiClient = new ApiClient();

  // src/store.ts
  var StateStore = class {
    constructor(api3 = apiClient, openEvents = () => new EventSource("/api/v1/events"), onError = () => {
    }) {
      this.api = api3;
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
  };
  var store = new StateStore();

  // src/native-page.ts
  var nativeState = async function() {
    if (!store.state) await store.reload();
    return store.state;
  };
  var nativePending = async () => {
    if (!store.observed) await store.start();
    return store.observed?.network_pending ?? null;
  };
  var devicePatch = (patch) => ({
    hostname: null,
    ndi_alias: null,
    mode: null,
    decode_mode: null,
    watchdog_mode: null,
    ntp_server: null,
    reboot_schedule: null,
    about: null,
    analog_audio_in_gain: null,
    analog_audio_out_gain: null,
    plugin_feeds: null,
    allow_insecure_feeds: null,
    web_allowed_hosts: null,
    ...patch
  });
  var nativeConfirm = async (title, text, options) => {
    const accepted = await confirmDialog(title, text, options);
    const modal = document.getElementById("confirm-modal");
    if (modal?.style.display && modal.style.display !== "none") {
      await new Promise((resolve) => modal.addEventListener("hidden.bs.modal", () => resolve(), { once: true }));
    }
    return accepted;
  };
  var nativeCommand = async function(command, revision, options = {}) {
    try {
      const expect_revision = revision ?? (await nativeState()).revision;
      apiClient.setCsrf(globalThis.ndimonCsrf || null);
      let cancelled = false;
      const reply = await apiClient.command(
        { command, expect_revision, ...options.expect_impact ? { expect_impact: options.expect_impact } : {} },
        options.confirm === false ? void 0 : async (impact) => {
          const text = impact.lines.join(" ") || "This change briefly interrupts the device.";
          const accepted = await nativeConfirm("Apply change", text, { label: "Apply" });
          cancelled = !accepted;
          return accepted;
        }
      ).catch((error) => {
        if (cancelled) return null;
        throw error;
      });
      if (!reply) return { ok: false, error: "Not applied", cancelled: true };
      for (const [key, value] of editRevisions) if (value === expect_revision) editRevisions.set(key, reply.revision);
      try {
        await store.reload();
      } catch {
      }
      return { ok: true, revision: reply.revision, result: reply.result };
    } catch (error) {
      if (!(error instanceof ApiFailure)) return { ok: false, error: "Could not reach the device" };
      const refusal = error.envelope.error;
      if (error.status === 401) {
        showLogin();
        return { ok: false, error: "Signed out" };
      }
      if (typeof refusal === "string") return { ok: false, error: refusal };
      if ("confirmation_required" in refusal) return { ok: false, error: "confirmation_required", impact: refusal.confirmation_required.impact };
      if ("revision_conflict" in refusal) return { ok: false, error: "revision_conflict", revision: refusal.revision_conflict.current_revision };
      if ("locked" in refusal) return { ok: false, error: refusal.locked.field === "matrix_confirmation_required" ? "matrix_confirmation_required" : "locked", errors: [refusal.locked.field] };
      if ("invalid" in refusal) return { ok: false, error: "invalid", errors: refusal.invalid.errors.map((e) => e.field + ": " + e.reason) };
      if ("unsupported" in refusal) return { ok: false, error: refusal.unsupported.what };
      if ("apply_failed" in refusal) return { ok: false, error: refusal.apply_failed.reason };
      return { ok: false, error: error.envelope.message };
    }
  };
  var confirmedCommand = async function(command, wording) {
    let token;
    for (let attempt = 0; ; attempt++) {
      const result = await nativeCommand(command, void 0, { confirm: false, ...token ? { expect_impact: token } : {} });
      const impact = result.impact;
      if (result.error !== "confirmation_required" || !impact?.token || attempt === 2) return result;
      if (attempt > 0) {
        const text = impact.lines?.join(" ") || "This change briefly interrupts the device.";
        if (!await nativeConfirm(wording.title, text, { label: wording.label, ...wording.danger ? { danger: true } : {} })) {
          return { ok: false, error: "Not applied", cancelled: true };
        }
      }
      token = impact.token;
    }
  };
  var nativeAudio = async function(ch) {
    const state = await nativeState();
    const bus = state.audio.outputs[ch] ?? { enabled: false, primary: false, inputs: [], destinations: [], legacy_matrix: false, definitions: { sources: {}, sinks: {} } };
    return {
      ...bus,
      revision: state.revision,
      native: bus,
      ...bus.legacy_matrix ? { warning: "Legacy per-destination gains and mutes are preserved. Changing this mix requires explicit normalization." } : {}
    };
  };
  var transportValues = { rudp: "RUDP", tcp: "TCP", multi_tcp: "M-TCP", udp: "UDP", multicast: "Multicast" };
  var transportFromLegacy = (value) => Object.keys(transportValues).find((key) => transportValues[key] === value) ?? "rudp";
  var loadNativeSettings = async function() {
    const state = await nativeState();
    setVal("tp-sel", transportValues[state.ndi.receive]);
    setVal("decode-mode-sel", state.device.decode_mode);
    setVal("disc-ip", state.ndi.discovery_servers.join(","));
    setNdiGroupsFromString(state.ndi.groups.join(","));
    setVal("off-ips", state.ndi.off_subnet_sources.join(","));
    const advertise = document.getElementById("ds-advertise");
    const control = document.getElementById("ds-control");
    if (advertise) advertise.checked = state.ndi.receiver_advertising;
    if (control) control.checked = state.ndi.remote_control;
    setVal("ndi-alias", state.device.ndi_alias);
    acceptNativeRevision("decoder", state.revision);
    acceptNativeRevision("discovery", state.revision);
  };
  var editRevisions = /* @__PURE__ */ new Map();
  var nativeRevision = async (key) => editRevisions.get(key) ?? (await nativeState()).revision;
  var acceptNativeRevision = (key, revision) => {
    if (revision !== void 0) editRevisions.set(key, revision);
  };
  document.addEventListener("DOMContentLoaded", () => {
    const remember = (event) => {
      const target = event.target;
      const card = target.closest?.("[data-card-idx]");
      const id = target.id;
      const key = card && /^(output-alias-|res-|scale-sel-|rotation-sel-)/.test(id) ? (id.startsWith("output-alias-") ? "alias:" : "video:") + (liveView().outputs[Number(card.dataset.cardIdx)]?.output_index ?? card.dataset.cardIdx) : ["tp-sel", "decode-mode-sel"].includes(id) ? "decoder" : ["disc-ip", "off-ips", "ndi-alias", "ndi-grp-input", "ds-advertise", "ds-control"].includes(id) || target.closest?.("#ndi-grp-tags") ? "discovery" : "";
      if (key && store.state && !editRevisions.has(key)) editRevisions.set(key, store.state.revision);
    };
    document.addEventListener("input", remember);
    document.addEventListener("change", remember);
    document.addEventListener("click", remember);
  });
  var resultText = (result, fallback) => result.error === "invalid" && result.errors?.length ? result.errors.join(" \xB7 ") : result.error || fallback;
  var applyTelemetry = function(data) {
    store.setTelemetry({ system: data.system, plugins: data.plugins });
  };
  var renderTelemetry = function() {
    const view = liveView();
    updateDash();
    renderTemps("dash-temps");
    renderTemps("sys-temps");
    setText("sys-up", view.uptime || "\u2014");
    setText("sys-plat", view.platform || "\u2014");
  };
  var cardClean = (...ids) => ids.every((id) => !document.getElementById(id)?.closest(".card")?.classList.contains("dirty"));
  var lastPluginProgress;
  var renderFromStore = function(domains) {
    const state = store.state;
    if (!state) return;
    const progress = store.status.services;
    if (progress?.type === "plugin_progress" && progress !== lastPluginProgress) {
      lastPluginProgress = progress;
      toast(`${progress.name}: ${progress.step}${progress.error ? " \xB7 " + progress.error : ""}`, progress.error ? "danger" : "info");
    }
    updateUI();
    if (domains.length) void refreshAudioFromServer();
    if (domains.includes("presets")) void loadPresets();
    if (domains.includes("ndi") || domains.includes("device")) {
      const ids = ["tp-sel", "decode-mode-sel", "disc-ip", "off-ips", "ndi-alias", "ds-advertise", "ds-control"];
      if (cardClean(...ids)) {
        editRevisions.delete("decoder");
        editRevisions.delete("discovery");
        void loadNativeSettings();
      }
    }
    if (domains.includes("display")) {
      if (cardClean("sp-bg-idle")) void loadSplashSettings();
      if (cardClean("osd-enabled")) void loadOsd();
    }
    if (domains.includes("device")) {
      if (cardClean("web-hosts")) void loadWebHosts();
      if (cardClean("ntp-server")) void loadNtp();
      if (cardClean("rb-enabled")) void loadRebootSchedule();
      if (cardClean("wd-mode")) void loadWatchdog();
    }
    if (domains.includes("network")) void loadNetwork();
    if (domains.includes("plugins")) void loadPlugins();
  };
  expose("nativeState", () => nativeState, (value) => {
    nativeState = value;
  });
  expose("nativeCommand", () => nativeCommand, (value) => {
    nativeCommand = value;
  });
  expose("nativeAudio", () => nativeAudio, (value) => {
    nativeAudio = value;
  });

  // src/auth.ts
  var failureText = (error, fallback) => typeof error.envelope.error === "string" ? error.envelope.error : error.envelope.message || fallback;
  var adoptSession = function(csrf) {
    apiClient.setCsrf(csrf);
    globalThis.ndimonCsrf = csrf || "";
  };
  var _booted2 = false;
  var startApp2 = async function() {
    const netNote = await consumeNetConfirm();
    let signedIn = true, defaultPassword = false;
    try {
      const session = await apiClient.session();
      if (session.csrf) adoptSession(session.csrf);
      signedIn = session.logged_in;
      defaultPassword = !!session.default_password;
    } catch {
    }
    if (!signedIn) {
      showLogin2();
      if (netNote) setText("login-err", netNote);
      return;
    }
    if (netNote) setTimeout(() => toast(netNote, netNote.startsWith("Network settings confirmed") ? "success" : "warning", 8e3), 400);
    hideLogin();
    if (defaultPassword) document.getElementById("pw-warning").style.display = "";
    if (_booted2) {
      resumeNetWait(true);
      return;
    }
    _booted2 = true;
    initSSE();
    loadInitialData();
    resumeNetWait(true);
    const cur = document.querySelector(".page.active")?.id.replace("page-", "");
    if (cur && cur !== "dashboard") loadPageData(cur);
  };
  var showLogin2 = function() {
    document.getElementById("login-overlay").style.display = "flex";
    setTimeout(() => document.getElementById("login-pw")?.focus(), 50);
  };
  var hideLogin = function() {
    document.getElementById("login-overlay").style.display = "none";
  };
  var doLogin = async function() {
    const pw = document.getElementById("login-pw").value;
    const err = document.getElementById("login-err");
    err.textContent = "";
    try {
      await apiClient.login(pw);
      adoptSession(apiClient.csrfToken);
      document.getElementById("login-pw").value = "";
      await startApp2();
    } catch (error) {
      err.textContent = error instanceof ApiFailure ? failureText(error, "login failed") : "network error";
    }
  };
  var doLogout = async function() {
    try {
      await apiClient.logout();
    } catch {
    }
    location.reload();
  };
  var changePassword = async function() {
    const cur = document.getElementById("pw-current").value;
    const nw = document.getElementById("pw-new").value;
    const cf = document.getElementById("pw-confirm").value;
    const msg = document.getElementById("pw-msg");
    if (nw !== cf) {
      msg.textContent = "passwords do not match";
      return;
    }
    try {
      await apiClient.changePassword(cur, nw);
    } catch (error) {
      msg.textContent = error instanceof ApiFailure ? failureText(error, "failed") : "failed";
      return;
    }
    adoptSession(apiClient.csrfToken);
    msg.textContent = "";
    ["pw-current", "pw-new", "pw-confirm"].forEach((id) => document.getElementById(id).value = "");
    document.getElementById("pw-warning").style.display = "none";
    toast("Password changed");
  };
  var loadWebHosts2 = async function() {
    try {
      const state = await nativeState();
      const input = document.getElementById("web-hosts");
      if (input) input.value = state.device.web_allowed_hosts.join(", ");
      acceptNativeRevision("security", state.revision);
    } catch {
    }
  };
  var saveWebHosts = async function() {
    const input = document.getElementById("web-hosts");
    const msg = document.getElementById("web-hosts-msg");
    const hosts = input.value.split(/[\s,]+/).map((h) => h.trim().toLowerCase()).filter(Boolean);
    const r = await nativeCommand({ set_device: devicePatch({ web_allowed_hosts: hosts }) }, await nativeRevision("security"));
    if (r && r.ok) {
      acceptNativeRevision("security", r.revision);
      msg.textContent = "";
      input.value = hosts.join(", ");
      toast("Host names saved");
    } else if (!r?.cancelled) {
      msg.textContent = resultText(r, "Save failed");
    }
  };
  expose("_booted", () => _booted2, (value) => {
    _booted2 = value;
  });
  expose("adoptSession", () => adoptSession, (value) => {
    adoptSession = value;
  });
  expose("startApp", () => startApp2, (value) => {
    startApp2 = value;
  });
  expose("showLogin", () => showLogin2, (value) => {
    showLogin2 = value;
  });
  expose("hideLogin", () => hideLogin, (value) => {
    hideLogin = value;
  });
  expose("doLogin", () => doLogin, (value) => {
    doLogin = value;
  });
  expose("doLogout", () => doLogout, (value) => {
    doLogout = value;
  });
  expose("changePassword", () => changePassword, (value) => {
    changePassword = value;
  });
  expose("loadWebHosts", () => loadWebHosts2, (value) => {
    loadWebHosts2 = value;
  });
  expose("saveWebHosts", () => saveWebHosts, (value) => {
    saveWebHosts = value;
  });

  // src/toast.ts
  var toast2 = function(msg, type, ms) {
    const el = document.getElementById("toast");
    el.className = `toast align-items-center text-bg-${type || "success"} border-0`;
    document.getElementById("toast-msg").textContent = msg;
    const delay = ms || (type === "danger" || type === "warning" ? 7e3 : 4e3);
    toastInst = bootstrap.Toast.getOrCreateInstance(el, { delay });
    toastInst._config.delay = delay;
    toastInst.show();
  };
  expose("toast", () => toast2, (value) => {
    toast2 = value;
  });

  // src/navigation.ts
  var PAGES2 = ["dashboard", "outputs", "ndi", "plugins", "settings", "system"];
  var showPage2 = function(name, opts = {}) {
    if (!PAGES2.includes(name)) name = "dashboard";
    document.querySelectorAll(".page").forEach((p) => p.classList.remove("active"));
    document.querySelectorAll(".nav-item").forEach((n) => n.classList.toggle("active", n.dataset.page === name));
    document.getElementById("page-" + name).classList.add("active");
    if (!opts.fromHash && location.hash !== "#" + name) history.replaceState(null, "", "#" + name);
    if (_booted) loadPageData2(name);
    window.scrollTo({ top: 0 });
  };
  var loadPageData2 = function(name) {
    if (name === "plugins") loadPlugins();
    if (name === "settings") {
      loadSettings();
      loadWebHosts();
    }
    if (name === "system") {
      loadSysInfo();
      loadWatchdog();
      loadRebootSchedule();
      loadSnapshots();
      loadNtp();
    }
    if (name === "outputs") {
      loadSourceIPs();
      loadPresets();
    }
    if (name === "ndi") loadSourceIPs();
  };
  expose("PAGES", () => PAGES2);
  expose("showPage", () => showPage2, (value) => {
    showPage2 = value;
  });
  expose("loadPageData", () => loadPageData2, (value) => {
    loadPageData2 = value;
  });

  // src/sse.ts
  var subscribed = false;
  var initSSE2 = function() {
    if (!subscribed) {
      subscribed = true;
      store.subscribe((_state, domains) => renderFromStore(domains));
      store.subscribeTelemetry(() => renderTelemetry());
      store.subscribeConnection((ok) => setConnState(ok));
    }
    void store.start().catch(() => setConnState(false));
  };
  var _connLost = false;
  var setConnState = function(ok) {
    if (_connLost === !ok) return;
    _connLost = !ok;
    const dot = document.getElementById("conn-dot");
    if (dot) dot.classList.toggle("lost", !ok);
    setText("conn-text", ok ? "Live" : "Reconnecting");
    const wrap = document.getElementById("conn-wrap");
    if (wrap) wrap.title = ok ? "Live updates connected" : "Lost contact with the device";
    const b = document.getElementById("sse-warning");
    if (b) b.style.display = ok ? "none" : "";
  };
  var telemetryPolling = null;
  var refreshTelemetry = async function() {
    try {
      const d = await api("/api/status");
      applyTelemetry(d);
      notifyPluginStatus(d);
    } catch {
    }
  };
  var startTelemetryPolling = function() {
    if (!telemetryPolling) telemetryPolling = setInterval(refreshTelemetry, 1500);
  };
  var _svcInterval = null;
  var startSvcPolling2 = function() {
    if (_svcInterval) return;
    _svcInterval = setInterval(() => {
      if (document.getElementById("page-system")?.classList.contains("active")) {
        loadServices();
        refreshWatchdogStats();
      }
    }, 1e4);
  };
  expose("initSSE", () => initSSE2, (value) => {
    initSSE2 = value;
  });
  expose("_connLost", () => _connLost, (value) => {
    _connLost = value;
  });
  expose("setConnState", () => setConnState, (value) => {
    setConnState = value;
  });
  expose("_svcInterval", () => _svcInterval, (value) => {
    _svcInterval = value;
  });
  expose("startSvcPolling", () => startSvcPolling2, (value) => {
    startSvcPolling2 = value;
  });

  // src/live.ts
  var emptyLiveView = () => ({
    outputs: [],
    sources: [],
    sourceIPs: {},
    connected: false,
    source_name: "",
    platform: "",
    total_frames: 0,
    dropped_frames: 0,
    audio_enabled: true,
    display_width: 0,
    display_height: 0,
    display_refresh: 0,
    display_refresh_hz: 0,
    device_name: "",
    cpu_percent: 0,
    memory: {},
    temps: {},
    uptime: "",
    decoder: {}
  });
  var memo;
  var liveView2 = function() {
    const key = [store.state, store.status.decoder, store.status.finder, store.telemetry];
    if (memo && memo.key.every((part, index) => part === key[index])) return memo.view;
    const state = store.state;
    const decoder = store.status.decoder ?? {};
    const finder = store.status.finder ?? {};
    const system = store.telemetry?.system;
    const view = {
      ...emptyLiveView(),
      ...system ? { cpu_percent: system.cpu_percent, memory: system.memory, temps: system.temps, uptime: system.uptime } : {},
      ...store.telemetry?.plugins ? { plugins: store.telemetry.plugins } : {}
    };
    if (state) {
      const observed = decoder.outputs ?? [];
      view.outputs = observed.map((live) => {
        const desired = state.outputs[live.output_index + 1];
        return {
          ...live,
          output_index: live.output_index,
          output_alias: desired?.alias || live.output_alias || "",
          source_name: live.source_name ?? desired?.source?.name ?? "",
          scale_mode: desired?.video.scale_mode ?? live.scale_mode ?? "letterbox",
          rotation: desired?.video.rotation ?? live.rotation ?? 0
        };
      });
      view.sourceIPs = Object.fromEntries(Object.entries(finder.list ?? {}).filter(([name]) => name !== "None"));
      view.sources = Object.keys(view.sourceIPs);
      view.connected = decoder.connected ?? false;
      view.source_name = decoder.source_name ?? "";
      view.platform = decoder.platform ?? "";
      view.total_frames = decoder.total_frames ?? 0;
      view.dropped_frames = decoder.dropped_frames ?? 0;
      view.audio_enabled = decoder.audio_enabled ?? true;
      view.display_width = decoder.width ?? decoder.display_width ?? 0;
      view.display_height = decoder.height ?? decoder.display_height ?? 0;
      view.display_refresh = decoder.refresh ?? decoder.display_refresh ?? 0;
      view.display_refresh_hz = decoder.refresh_hz ?? decoder.display_refresh_hz ?? 0;
      view.device_name = state.device.ndi_alias || decoder.device_name || "";
      view.decoder = {
        tally: state.ndi.tally === "on" ? "TallyOn" : state.ndi.tally === "video" ? "TallyVideo" : "TallyOff",
        audio: state.audio.outputs[1]?.enabled ? "NDIAudioEn" : "NDIAudioDis"
      };
    }
    memo = { key, view };
    return view;
  };
  expose("liveView", () => liveView2, (value) => {
    liveView2 = value;
  });

  // src/initial-load.ts
  var loadInitialData2 = async function() {
    try {
      await store.start();
    } catch {
    }
    await refreshTelemetry();
    startTelemetryPolling();
    loadSourceIPs2();
    loadSettings();
    loadWebHosts();
    loadVersionTag();
    startSvcPolling();
  };
  var loadVersionTag = async function() {
    try {
      const d = await api("/v1/AboutMe");
      setText("sb-version", d.FirmwareVersion ? "v" + String(d.FirmwareVersion).replace(/^v/, "") : "\u2014");
      if (d.Model) setText("sb-platform", d.Model);
    } catch {
    }
  };
  var loadSourceIPs2 = async function() {
    try {
      await store.refreshSources();
    } catch {
    }
  };
  var fmtHz2 = function(hz) {
    if (!hz) return "\u2014";
    return Math.round(hz * 100) / 100 + " Hz";
  };
  var outputModes = {};
  var sourceLabel = (out) => (out.source_name || "None") + (out.source_name && out.source_origin === "discovery_server" ? " \xB7 set by Discovery Server" : "");
  var loadOutputModes = async function(cardIdx, outputIndex) {
    try {
      const catalog = await apiClient.get(`/api/v1/catalog/display_modes?output=${outputIndex + 1}`);
      const video = (await nativeState()).outputs[outputIndex + 1]?.video;
      const d = { ...catalog, ...video ? {
        auto_resolution: video.resolution === "auto",
        rotation: video.rotation,
        ...video.resolution !== "auto" ? { current: video.resolution.mode } : {}
      } : {} };
      outputModes[outputIndex] = d.modes || [];
      const cur = d.current || {};
      const resMap = /* @__PURE__ */ new Map();
      outputModes[outputIndex].forEach((m) => {
        const key = m.width + "x" + m.height;
        if (!resMap.has(key)) resMap.set(key, { width: m.width, height: m.height, preferred: m.preferred });
        else if (m.preferred) resMap.get(key).preferred = true;
      });
      const whSel = document.getElementById(`res-wh-${cardIdx}`);
      if (!whSel) return;
      whSel.innerHTML = "";
      const autoOpt = document.createElement("option");
      autoOpt.value = "auto";
      autoOpt.dataset.auto = "1";
      autoOpt.textContent = "Auto (match source)";
      if (d.auto_resolution) autoOpt.selected = true;
      whSel.appendChild(autoOpt);
      resMap.forEach((r) => {
        const o = document.createElement("option");
        o.value = r.width + "x" + r.height;
        o.dataset.w = r.width;
        o.dataset.h = r.height;
        o.textContent = `${r.width} \xD7 ${r.height}${r.preferred ? " \u2605" : ""}`;
        if (!d.auto_resolution && r.width === cur.width && r.height === cur.height) o.selected = true;
        whSel.appendChild(o);
      });
      populateOutputRefreshRates(cardIdx, outputIndex, cur);
      const fpsCol = document.getElementById(`res-fps-${cardIdx}`)?.parentElement;
      if (fpsCol && d.auto_resolution) fpsCol.style.display = "none";
      if (Array.isArray(d.supported_rotations)) {
        const select = document.getElementById(`rotation-sel-${cardIdx}`);
        if (select) for (const option of select.options) option.disabled = !d.supported_rotations.includes(Number(option.value));
      }
      if (d.rotation !== void 0) setVal(`rotation-sel-${cardIdx}`, String(d.rotation));
    } catch {
    }
  };
  var populateOutputRefreshRates = function(cardIdx, outputIndex, cur) {
    const whSel = document.getElementById(`res-wh-${cardIdx}`);
    const fpsSel = document.getElementById(`res-fps-${cardIdx}`);
    if (!whSel || !fpsSel) return;
    const opt = whSel.options[whSel.selectedIndex];
    if (!opt) return;
    const w = parseInt(opt.dataset.w), h = parseInt(opt.dataset.h);
    const rates = (outputModes[outputIndex] || []).filter((m) => m.width === w && m.height === h);
    fpsSel.innerHTML = "";
    rates.forEach((m) => {
      const o = document.createElement("option");
      o.value = m.refresh_hz || m.refresh;
      o.dataset.hz = m.refresh_hz || m.refresh;
      o.textContent = fmtHz2(m.refresh_hz || m.refresh) + (m.preferred ? " \u2605" : "");
      const curHz = cur ? cur.refresh_hz || cur.refresh : 0;
      if (curHz && Math.abs((m.refresh_hz || m.refresh) - curHz) < 0.02) o.selected = true;
      fpsSel.appendChild(o);
    });
  };
  var onResChange = function(cardIdx) {
    const out = liveView2().outputs[cardIdx];
    const whSel = document.getElementById(`res-wh-${cardIdx}`);
    const fpsCol = document.getElementById(`res-fps-${cardIdx}`)?.parentElement;
    const isAuto = whSel?.options[whSel.selectedIndex]?.dataset.auto === "1";
    if (fpsCol) fpsCol.style.display = isAuto ? "none" : "";
    if (!isAuto) populateOutputRefreshRates(cardIdx, out?.output_index ?? cardIdx, {});
  };
  var updateNdiPreview = function(cardIdx) {
    const view = liveView2();
    if (cardIdx === void 0) {
      (view.outputs || []).forEach((_, i) => updateNdiPreview(i));
      updateSplashPreview();
      return;
    }
    const preview = document.getElementById(`ndi-preview-${cardIdx}`);
    if (!preview) return;
    const devName = document.getElementById("ndi-alias")?.value.trim() || view.device_name || "NDIMON-XXXX";
    const out = view.outputs[cardIdx];
    const outAlias = document.getElementById(`output-alias-${cardIdx}`)?.value.trim() || out?.output_alias || out?.connector?.replace("HDMI-A-", "HDMI-") || `HDMI-${cardIdx + 1}`;
    preview.textContent = `NDI name: "${devName} (${outAlias})"`;
  };
  var outputExpansionState = /* @__PURE__ */ new Map();
  var outputExpansionKey = function(out, idx) {
    return "output:" + (out.output_index !== void 0 ? out.output_index : out.connector || idx);
  };
  var outputExpansionOpen = function(key, fallback = true) {
    if (outputExpansionState.has(key)) return outputExpansionState.get(key);
    if (key.endsWith(":advanced")) return fallback;
    try {
      const saved = localStorage.getItem("ndimon.outputs.expansion." + key);
      if (saved === "open" || saved === "closed") return saved === "open";
    } catch (_) {
    }
    return fallback;
  };
  var rememberOutputExpansion = function(details) {
    const key = details.dataset.expansionKey;
    if (!key) return;
    outputExpansionState.set(key, details.open);
    if (key.endsWith(":advanced")) return;
    try {
      localStorage.setItem("ndimon.outputs.expansion." + key, details.open ? "open" : "closed");
    } catch (_) {
    }
  };
  document.addEventListener("DOMContentLoaded", () => {
    for (const [key, fallback] of [["connections", true], ["presets", false]]) {
      const section = document.querySelector('[data-expansion-key="' + key + '"]');
      if (section) section.open = outputExpansionOpen(key, fallback);
    }
  });
  var outputAudioDrafts = /* @__PURE__ */ new Map();
  var outputAudioCatalog = { inputs: [], destinations: [] };
  var outputAudioEngine = "unknown";
  var outputAudioCatalogPending;
  var outputAudioChannel = function(out) {
    return Number(out.output_index) + 1;
  };
  var outputAudioError = function(result) {
    if (result.error === "legacy_migration_required") return "Existing audio cannot be migrated safely into this editor. No settings were changed. " + (result.errors || []).join(" \xB7 ");
    return [result.error, ...Array.isArray(result.errors) ? result.errors : []].filter(Boolean).join(" \xB7 ") || "Audio request failed";
  };
  var loadOutputAudioCatalog = async function(force = false) {
    if (!force && outputAudioCatalogPending) return outputAudioCatalogPending;
    outputAudioCatalogPending = Promise.all([apiClient.get("/api/v1/catalog/audio_endpoints"), nativeState()]).then(([catalog, state]) => {
      if (catalog.ok === false || !Array.isArray(catalog.inputs) || !Array.isArray(catalog.destinations)) throw Error(outputAudioError(catalog));
      outputAudioCatalog = catalog;
      outputAudioEngine = state.audio.engine;
    }).catch((error) => {
      outputAudioCatalogPending = void 0;
      throw error;
    });
    return outputAudioCatalogPending;
  };
  var outputAudioDraft = function(data) {
    return { native: data.native, revision: data.revision, legacy_matrix: data.legacy_matrix === true, warning: data.warning || "", enabled: data.enabled === true, inputs: (data.inputs || []).map((row) => ({ ...row })), destinations: (data.destinations || []).map((row) => ({ ...row })), loaded: true, dirty: false, busy: false, error: "" };
  };
  var validateOutputAudio = function(draft) {
    if (draft.enabled && (!draft.inputs.length || !draft.destinations.length)) throw Error("Select at least one input and one destination before enabling this output.");
    for (const kind of ["inputs", "destinations"]) {
      const ids = /* @__PURE__ */ new Set();
      for (const row of draft[kind]) {
        if (!row.endpoint) throw Error("Choose an endpoint for every " + (kind === "inputs" ? "input" : "destination") + ".");
        if (ids.has(row.endpoint)) throw Error("Each endpoint may be selected only once in this output.");
        ids.add(row.endpoint);
        if (!Number.isInteger(row.delay_ms) || row.delay_ms < 0 || row.delay_ms > 5e3) throw Error("Every audio delay must be a whole number from 0 to 5000 ms.");
        if (kind === "inputs" && (!Number.isFinite(row.gain_db) || row.gain_db < -60 || row.gain_db > 12)) throw Error("Input gain must be between \u221260 and +12 dB.");
      }
    }
    if (draft.enabled) {
      for (const input of draft.inputs) for (const destination of draft.destinations) if (input.delay_ms + destination.delay_ms > 5e3) throw Error("Input delay plus destination delay must not exceed 5000 ms for any route.");
    }
  };
  var outputAudioPayload = function(draft) {
    validateOutputAudio(draft);
    return { revision: draft.revision, enabled: draft.enabled, inputs: draft.inputs.map(({ endpoint, gain_db, delay_ms }) => ({ endpoint, gain_db, delay_ms })), destinations: draft.destinations.map(({ endpoint, delay_ms }) => ({ endpoint, delay_ms })) };
  };
  var mountOutputAudio = async function(out) {
    const ch = outputAudioChannel(out), pane = document.getElementById("output-audio-" + ch);
    if (!pane || !Number.isInteger(ch) || ch < 1) return;
    let draft = outputAudioDrafts.get(ch);
    if (draft?.loaded) {
      renderOutputAudio(ch);
      return;
    }
    if (draft?.loading) return;
    draft = { loading: true, error: "" };
    outputAudioDrafts.set(ch, draft);
    pane.textContent = "Loading audio settings\u2026";
    try {
      const [, data] = await Promise.all([loadOutputAudioCatalog(), nativeAudio(ch)]);
      if (data.ok === false || !Array.isArray(data.inputs) || !Array.isArray(data.destinations)) throw Error(outputAudioError(data));
      outputAudioDrafts.set(ch, outputAudioDraft(data));
    } catch (error) {
      outputAudioDrafts.set(ch, { loaded: false, error: error.message });
    }
    renderOutputAudio(ch);
  };
  var renderOutputAudio = function(ch) {
    const pane = document.getElementById("output-audio-" + ch), draft = outputAudioDrafts.get(ch);
    if (!pane || !draft) return;
    pane.replaceChildren();
    const node = (tag, text, className) => {
      const el = document.createElement(tag);
      if (text !== void 0) el.textContent = text;
      if (className) el.className = className;
      return el;
    };
    const button = (text, handler, className = "btn btn-outline-secondary btn-sm") => {
      const el = node("button", text, className);
      el.type = "button";
      el.disabled = !!draft.busy;
      el.onclick = handler;
      return el;
    };
    const message = node("div", draft.error || "", "alert alert-danger");
    message.hidden = !draft.error;
    message.setAttribute("role", "alert");
    pane.append(message);
    if (draft.stale) pane.append(node("div", "Audio changed in another session or on the device. Reload saved audio to see the current settings; saving these edits would be refused.", "alert alert-warning"));
    const v1Note = globalThis.ndimonCsrf ? " \xB7 Saving enabled routes switches to audio engine v2; the device asks before restarting the decoder." : " \xB7 Routing can be saved now; it becomes active only after explicitly switching to engine v2.";
    pane.append(node("p", "Audio engine: " + outputAudioEngine + (outputAudioEngine === "v1" ? v1Note : ""), "form-text"));
    const targetEngine = outputAudioEngine === "v2" ? "v1" : "v2";
    pane.append(button(targetEngine === "v2" ? "Use experimental audio engine v2\u2026" : "Return to audio engine v1\u2026", async () => {
      const title = targetEngine === "v2" ? "Enable audio engine v2" : "Return to audio engine v1";
      const message2 = targetEngine === "v2" ? "Switch to experimental audio engine v2 and restart the decoder? Video may pause briefly. Saved routes for enabled outputs will become active." : "Return to audio engine v1 and restart the decoder? Video may pause briefly. Per-output network audio routes will become inactive; saved selections are kept.";
      if (!await nativeConfirm(title, message2, { label: "Switch and restart" })) return;
      draft.busy = true;
      renderOutputAudio(ch);
      try {
        const result = await nativeCommand({ set_audio_engine: { engine: targetEngine } });
        if (result.ok === false || result.error) throw Error(outputAudioError(result));
        outputAudioEngine = targetEngine;
        draft.error = "";
        toast("Audio engine " + targetEngine + " selected; decoder restarting", "success");
      } catch (error) {
        draft.error = error.message;
      } finally {
        draft.busy = false;
        for (const [output] of outputAudioDrafts) renderOutputAudio(output);
      }
    }));
    if (!draft.loaded) {
      pane.append(button("Retry audio settings", () => {
        outputAudioDrafts.delete(ch);
        mountOutputAudio(liveView2().outputs.find((out) => outputAudioChannel(out) === ch) || { output_index: ch - 1 });
      }));
      return;
    }
    if (draft.legacy_matrix) pane.append(node("p", draft.warning || "Existing per-destination gains and mutes are preserved. Edits that would replace this mix are blocked until explicitly reviewed.", "alert alert-warning"));
    const edit = () => {
      draft.dirty = true;
    };
    const state = node("p", draft.enabled ? "This output\u2019s audio routes are enabled." : "Disabled: this output contributes no audio. Saved selections are retained.", "form-text");
    const enableLabel = node("label", void 0, "form-check-label mb-2");
    const enable = node("input");
    enable.type = "checkbox";
    enable.className = "form-check-input me-2";
    enable.checked = draft.enabled;
    enable.disabled = !!draft.busy;
    enable.onchange = () => {
      draft.enabled = enable.checked;
      edit();
      state.textContent = draft.enabled ? "This output\u2019s audio routes will be enabled when saved." : "Disabled: this output contributes no audio. Saved selections are retained.";
    };
    enableLabel.append(enable, document.createTextNode("Enable audio for this output"));
    pane.append(enableLabel, state);
    pane.append(node("p", "Each input feeds every selected destination. Input delay and destination delay add together. Adjust HDMI and Tessera timing manually; automatic synchronization is not provided.", "form-text"));
    for (const kind of ["inputs", "destinations"]) {
      const input = kind === "inputs";
      pane.append(node("h6", input ? "Audio inputs" : "Audio destinations", "mt-3"));
      const list = node("div");
      pane.append(list);
      draft[kind].forEach((row, index) => {
        const wrapper = node("div", void 0, "border rounded p-2 mb-2");
        const grid = node("div", void 0, "row g-2 align-items-end");
        wrapper.append(grid);
        list.append(wrapper);
        const selectCol = node("div", void 0, "col-md-6");
        const selectLabel = node("label", (input ? "Input" : "Destination") + " " + (index + 1), "form-label w-100");
        const select = node("select");
        select.className = "form-select form-select-sm";
        select.disabled = !!draft.busy;
        select.required = true;
        const choices = outputAudioCatalog[kind] || [];
        const blank = node("option", "Choose " + (input ? "input" : "destination") + "\u2026");
        blank.value = "";
        select.append(blank);
        const options = choices.some((endpoint) => endpoint.id === row.endpoint) || !row.endpoint ? choices : [...choices, { id: row.endpoint, label: row.endpoint, ready: false, reason: "Endpoint is absent from the current catalog" }];
        for (const endpoint of options) {
          const option = node("option", endpoint.label + (endpoint.ready ? "" : " \xB7 unavailable"));
          option.value = endpoint.id;
          select.append(option);
        }
        select.value = row.endpoint;
        const info = node("div", void 0, "form-text");
        const describe = () => {
          const endpoint = options.find((endpoint2) => endpoint2.id === select.value);
          info.textContent = endpoint ? [endpoint.mapping, endpoint.ready ? "Ready" : "Unavailable: " + (endpoint.reason || "not ready"), endpoint.channels ? endpoint.channels + " channel(s)" : ""].filter(Boolean).join(" \xB7 ") : "";
        };
        describe();
        select.onchange = () => {
          row.endpoint = select.value;
          edit();
          describe();
        };
        selectLabel.append(select);
        selectCol.append(selectLabel, info);
        grid.append(selectCol);
        const numeric = (field, label, min, max, step) => {
          const col = node("div", void 0, "col-md");
          const l = node("label", label, "form-label w-100");
          const el = node("input");
          el.type = "number";
          el.className = "form-control form-control-sm";
          el.min = String(min);
          el.max = String(max);
          el.step = String(step);
          el.required = true;
          el.value = row[field];
          el.disabled = !!draft.busy;
          el.oninput = () => {
            row[field] = el.value.trim() === "" ? NaN : Number(el.value);
            edit();
          };
          l.append(el);
          col.append(l);
          grid.append(col);
        };
        if (input) numeric("gain_db", "Gain (dB)", -60, 12, 0.1);
        numeric("delay_ms", input ? "Input delay (ms)" : "Destination delay (ms)", 0, 5e3, 1);
        const remove = node("div", void 0, "col-md-auto");
        const removeButton = button("Remove", () => {
          draft[kind].splice(index, 1);
          edit();
          renderOutputAudio(ch);
        });
        removeButton.setAttribute("aria-label", "Remove " + (input ? "input " : "destination ") + (index + 1) + " from output " + ch);
        remove.append(removeButton);
        grid.append(remove);
      });
      pane.append(button(input ? "Add audio input" : "Add audio destination", () => {
        draft[kind].push(input ? { endpoint: "", gain_db: 0, delay_ms: 0 } : { endpoint: "", delay_ms: 0 });
        edit();
        renderOutputAudio(ch);
      }));
    }
    const controls = node("div", void 0, "d-flex gap-2 flex-wrap mt-3");
    controls.append(button("Refresh endpoint names and availability", async () => {
      draft.busy = true;
      draft.error = "";
      renderOutputAudio(ch);
      try {
        await loadOutputAudioCatalog(true);
      } catch (error) {
        draft.error = error.message;
      } finally {
        draft.busy = false;
        renderOutputAudio(ch);
      }
    }));
    controls.append(button("Save output audio", () => saveOutputAudio(ch), "btn btn-primary btn-sm"), button("Reload saved audio", async () => {
      if (draft.dirty && !await confirmDialog("Reload saved audio", "Discard unsaved audio edits for this output?", { label: "Reload" })) return;
      outputAudioDrafts.delete(ch);
      outputAudioCatalogPending = void 0;
      try {
        await store.reload();
        await mountOutputAudio({ output_index: ch - 1 });
      } catch (error) {
        outputAudioDrafts.set(ch, { loaded: false, error: error.message });
        renderOutputAudio(ch);
      }
    }));
    pane.append(controls);
  };
  var refreshAudioFromServer2 = async function() {
    if (!outputAudioDrafts.size) return;
    try {
      await loadOutputAudioCatalog(true);
    } catch {
    }
    for (const [ch, draft] of outputAudioDrafts) {
      if (!draft.loaded || draft.busy) continue;
      let data;
      try {
        data = await nativeAudio(ch);
      } catch {
        continue;
      }
      if (data.ok === false || data.revision === void 0 || data.revision === draft.revision) continue;
      if (draft.dirty) draft.stale = true;
      else outputAudioDrafts.set(ch, outputAudioDraft(data));
      renderOutputAudio(ch);
    }
  };
  var saveOutputAudio = async function(ch) {
    const draft = outputAudioDrafts.get(ch);
    if (!draft?.loaded || draft.busy) return;
    let body;
    try {
      body = outputAudioPayload(draft);
    } catch (error) {
      draft.error = error.message;
      renderOutputAudio(ch);
      return;
    }
    draft.busy = true;
    draft.error = "";
    renderOutputAudio(ch);
    try {
      const audio = { ...draft.native ?? { primary: false, legacy_matrix: false, definitions: { sources: {}, sinks: {} } }, enabled: body.enabled, inputs: body.inputs, destinations: body.destinations };
      const command = { put_output_audio: { output: ch, audio, normalize_matrix: false } };
      let result = await nativeCommand(command, body.revision, { confirm: false }), confirmed = false;
      for (let round = 0; round < 2 && result.error === "confirmation_required" && result.impact?.token; round++) {
        const text = (result.impact.lines || []).join(" ") || "This change restarts the decoder. Video may pause briefly.";
        if (!await nativeConfirm("Apply audio change", text, { label: "Apply" })) return;
        result = await nativeCommand(command, body.revision, { confirm: false, expect_impact: result.impact.token });
        confirmed = true;
      }
      if (result.error === "matrix_confirmation_required") throw Error("Save blocked: this edit would replace existing per-destination gains or mutes and could make a muted destination audible. The existing mix is unchanged. " + (result.errors || []).join(" \xB7 "));
      if (result.error === "revision_conflict") draft.stale = true;
      if (result.ok === false || result.error) throw Error((result.error === "revision_conflict" ? "Audio changed in another session. Reload saved audio for this output before saving again. " : "") + outputAudioError(result));
      if (result.revision !== void 0) {
        for (const value of outputAudioDrafts.values()) if (value.revision === body.revision) value.revision = result.revision;
      }
      draft.dirty = false;
      draft.stale = false;
      if (confirmed) {
        try {
          await loadOutputAudioCatalog(true);
        } catch {
        }
      }
      toast("Output " + ch + " audio saved" + (outputAudioEngine === "v1" ? " (engine v1 remains active)" : ""), "success");
    } catch (error) {
      draft.error = error.message;
    } finally {
      draft.busy = false;
      renderOutputAudio(ch);
    }
  };
  var builtOutputCount = 0;
  var builtOutputKeys = "";
  var buildOutputCards = function() {
    const container = document.getElementById("outputs-container");
    const outputs = liveView2().outputs;
    if (!container || !outputs?.length) return;
    builtOutputCount = outputs.length;
    builtOutputKeys = outputs.map(outputExpansionKey).join("|");
    container.innerHTML = outputs.map((out, idx) => {
      const alias = out.output_alias || out.connector?.replace("HDMI-A-", "HDMI-") || `Output ${idx + 1}`;
      return `
    <details class="card mb-3" data-card-idx="${idx}" data-expansion-key="${esc(outputExpansionKey(out, idx))}" ${outputExpansionOpen(outputExpansionKey(out, idx)) ? "open" : ""} ontoggle="rememberOutputExpansion(this)">
      <summary class="card-header">
        <i class="bi bi-display"></i><span>Output ${idx + 1} \xB7 <span id="out-title-${idx}">${esc(alias)}</span></span>
        <span class="spacer"></span>
        <span id="out-chips-${idx}" class="d-flex gap-1"></span>
        <span id="out-badge-${idx}" class="chip ${out.connected ? "chip-live" : "chip-idle"}">${out.connected ? "LIVE" : "IDLE"}</span>
      </summary>
      <div class="card-body">
        <div class="sub-head"><i class="bi bi-broadcast-pin"></i>Source</div>
        <div class="row g-2 mb-2">
          <div class="col-12">
            <select class="form-select" id="src-sel-${idx}" onchange="onSrcChange(${idx})" aria-label="NDI source for output ${idx + 1}">
              <option value="">\u2014 Select source \u2014</option>
            </select>
          </div>
        </div>
        <div class="row g-2 mb-2" id="manual-ip-row-${idx}" style="display:none">
          <div class="col-7">
            <label class="form-label" for="manual-name-${idx}">Source Name</label>
            <input type="text" class="form-control form-control-sm" id="manual-name-${idx}" placeholder="MACHINE (Source Name)">
          </div>
          <div class="col-5">
            <label class="form-label" for="manual-ip-${idx}">IP Address <small>(optional)</small></label>
            <input type="text" class="form-control form-control-sm" id="manual-ip-${idx}" placeholder="192.168.1.100">
          </div>
        </div>
        <div class="save-row mb-3">
          <button class="btn btn-primary btn-sm" onclick="applySource(${idx})"><i class="bi bi-plug me-1"></i>Connect</button>
          <button class="btn btn-outline-secondary btn-sm" onclick="doDisconnect(${idx})" id="out-disc-${idx}"><i class="bi bi-x-lg me-1"></i>Disconnect</button>
        </div>

        <div class="row g-0 mb-3" style="background:var(--bg-card2);border:1px solid var(--border);border-radius:var(--radius-sm);padding:4px 12px">
          <div class="col-12"><div class="stat-row"><span class="stat-label">Now showing</span><span class="stat-val" id="out-src-${idx}">${esc(sourceLabel(out))}</span></div></div>
          <div class="col-12"><div class="stat-row"><span class="stat-label">Input \u2192 Output</span><span class="stat-val" id="out-res-${idx}">\u2014</span></div></div>
          <div class="col-12"><div class="stat-row"><span class="stat-label">Frame rate \xB7 Codec</span><span class="stat-val" id="out-fps-${idx}">\u2014</span></div></div>
        </div>

        <div class="sub-head"><i class="bi bi-tag"></i>Name</div>
        <div class="row g-2 mb-3 align-items-end">
          <div class="col-8">
            <input type="text" class="form-control form-control-sm" id="output-alias-${idx}"
                   value="${esc(out.output_alias || "")}" aria-label="Output name"
                   placeholder="${esc(out.connector?.replace("HDMI-A-", "HDMI-") || "HDMI-" + (idx + 1))}"
                   oninput="updateNdiPreview(${idx})">
            <div class="form-text" id="ndi-preview-${idx}"></div>
          </div>
          <div class="col-4 d-grid">
            <button class="btn btn-outline-primary btn-sm" onclick="saveOutputName(${idx})"><i class="bi bi-check-lg me-1"></i>Save Name</button>
          </div>
        </div>

        <details class="output-advanced" data-expansion-key="${esc(outputExpansionKey(out, idx))}:advanced" ${outputExpansionOpen(outputExpansionKey(out, idx) + ":advanced", false) ? "open" : ""} ontoggle="rememberOutputExpansion(this)">
          <summary>Advanced</summary>
          <div class="advanced-body">
        <div class="sub-head"><i class="bi bi-aspect-ratio"></i>Video</div>
        <div class="row g-2 mb-2">
          <div class="col-7">
            <label class="form-label" for="res-wh-${idx}">Resolution</label>
            <select class="form-select form-select-sm" id="res-wh-${idx}" onchange="onResChange(${idx})"><option>Loading\u2026</option></select>
          </div>
          <div class="col-5">
            <label class="form-label" for="res-fps-${idx}">Refresh Rate</label>
            <select class="form-select form-select-sm" id="res-fps-${idx}"><option>\u2014</option></select>
          </div>
        </div>
        <div class="row g-2 mb-3">
          <div class="col-8">
            <label class="form-label" for="scale-sel-${idx}">Scale Mode</label>
            <select class="form-select form-select-sm" id="scale-sel-${idx}">
              <option value="letterbox">Letterbox (keep aspect ratio)</option>
              <option value="stretch">Stretch (fill screen)</option>
              <option value="crop">Crop (fill, no bars)</option>
            </select>
          </div>
          <div class="col-4">
            <label class="form-label" for="rotation-sel-${idx}">Rotation</label>
            <select class="form-select form-select-sm" id="rotation-sel-${idx}">
              <option value="0">0&deg;</option>
              <option value="90">90&deg;</option>
              <option value="180">180&deg;</option>
              <option value="270">270&deg;</option>
            </select>
          </div>
        </div>
        <div class="save-row">
          <button class="btn btn-primary btn-sm" onclick="saveVideoSettings(${idx})"><i class="bi bi-check-lg me-1"></i>Save Video Settings</button>
          <span id="out-msg-${idx}" class="save-msg"></span>
        </div>
        <hr class="section-divider">
        <div class="sub-head"><i class="bi bi-volume-up"></i>Audio</div>
        <div id="output-audio-${outputAudioChannel(out)}">Loading audio settings\u2026</div>
          </div>
        </details>
      </div>
    </details>`;
    }).join("");
    outputs.forEach((out, idx) => {
      acceptNativeRevision("alias:" + out.output_index, store.state?.revision);
      acceptNativeRevision("video:" + out.output_index, store.state?.revision);
      setVal(`scale-sel-${idx}`, out.scale_mode || "letterbox");
      setVal(`rotation-sel-${idx}`, String(out.rotation || 0));
      populateOutputSrcSelector(idx);
      loadOutputModes(idx, out.output_index);
      updateNdiPreview(idx);
      mountOutputAudio(out);
    });
  };
  var updateOutputCards2 = function() {
    const view = liveView2();
    if (!view.outputs?.length) return;
    if (builtOutputCount !== view.outputs.length || builtOutputKeys !== view.outputs.map(outputExpansionKey).join("|")) {
      buildOutputCards();
      return;
    }
    view.outputs.forEach((out, idx) => {
      const badge = document.getElementById(`out-badge-${idx}`);
      if (badge) {
        badge.className = `chip ${out.connected ? "chip-live" : "chip-idle"}`;
        badge.textContent = out.connected ? "LIVE" : "IDLE";
      }
      const chips = document.getElementById(`out-chips-${idx}`);
      if (chips) chips.innerHTML = decodeChips(out);
      const disc = document.getElementById(`out-disc-${idx}`);
      if (disc) disc.disabled = !out.connected && !out.source_name;
      setText(`out-src-${idx}`, sourceLabel(out));
      const inStr = out.input_width ? `${out.input_width}\xD7${out.input_height}` : "\u2014";
      const outStr = out.width ? `${out.width}\xD7${out.height} @ ${fmtHz2(out.refresh_hz || out.refresh)}` : "\u2014";
      setText(`out-res-${idx}`, `${inStr} \u2192 ${outStr}`);
      setText(`out-fps-${idx}`, (out.fps > 0 ? out.fps.toFixed(1) + " fps" : "\u2014") + " \xB7 " + fmtCodec(out.codec));
    });
    const srcKey = JSON.stringify(view.sources);
    if (srcKey !== _lastSrcKey) {
      _lastSrcKey = srcKey;
      view.outputs.forEach((out, idx) => populateOutputSrcSelector(idx));
      populatePresetSourceSelect();
    }
  };
  var _lastSrcKey = "";
  var populateOutputSrcSelector = function(cardIdx) {
    const sel = document.getElementById(`src-sel-${cardIdx}`);
    if (!sel) return;
    const view = liveView2();
    const cur = view.outputs[cardIdx]?.source_name || "";
    const prev = sel.value || cur;
    const isManual = prev === "__manual__";
    sel.innerHTML = "";
    const noneOpt = document.createElement("option");
    noneOpt.value = "None";
    noneOpt.textContent = "None (Disconnect)";
    sel.appendChild(noneOpt);
    let foundPrev = prev === "None";
    view.sources.forEach((s) => {
      const o = document.createElement("option");
      o.value = s;
      o.textContent = s;
      sel.appendChild(o);
      if (s === prev) foundPrev = true;
    });
    const manOpt = document.createElement("option");
    manOpt.value = "__manual__";
    manOpt.textContent = "Manual...";
    sel.appendChild(manOpt);
    if (isManual) foundPrev = true;
    if (!foundPrev && prev && prev !== "None") {
      const curOpt = document.createElement("option");
      curOpt.value = prev;
      curOpt.textContent = prev + " (not discovered)";
      sel.insertBefore(curOpt, manOpt);
    }
    sel.value = prev;
    onSrcChange2(cardIdx);
  };
  var onSrcChange2 = function(cardIdx) {
    const sel = document.getElementById(`src-sel-${cardIdx}`);
    const manRow = document.getElementById(`manual-ip-row-${cardIdx}`);
    if (manRow) manRow.style.display = sel?.value === "__manual__" ? "" : "none";
  };
  var saveOutputName = async function(cardIdx) {
    const out = liveView2().outputs[cardIdx];
    const outputIndex = out?.output_index ?? cardIdx;
    const outAlias = (document.getElementById(`output-alias-${cardIdx}`)?.value || "").trim();
    const r = await nativeCommand({ set_output_alias: { output: outputIndex + 1, alias: outAlias } }, await nativeRevision("alias:" + outputIndex));
    if (r.ok) acceptNativeRevision("alias:" + outputIndex, r.revision);
    done(r, `Output ${cardIdx + 1}: name saved`);
  };
  var applySource = async function(cardIdx) {
    const view = liveView2();
    const out = view.outputs[cardIdx];
    const outputIndex = out?.output_index ?? cardIdx;
    const srcSel = document.getElementById(`src-sel-${cardIdx}`);
    const srcVal = srcSel?.value || "";
    if (srcVal === "__manual__") {
      const manName = (document.getElementById(`manual-name-${cardIdx}`)?.value || "").trim();
      const manIP = (document.getElementById(`manual-ip-${cardIdx}`)?.value || "").trim();
      if (!manName) {
        toast("Enter a source name for manual connect", "warning");
        return;
      }
      const r = await nativeCommand(manName === "None" ? { forget_source: { output: outputIndex + 1 } } : { select_source: { output: outputIndex + 1, source: { name: manName, endpoint: manIP } } });
      done(r, `Output ${cardIdx + 1}: connecting to ${manName}\u2026`);
    } else if (!srcVal) {
      toast("Pick a source first", "warning");
    } else {
      const r = await nativeCommand(srcVal === "None" ? { forget_source: { output: outputIndex + 1 } } : { select_source: { output: outputIndex + 1, source: { name: srcVal, endpoint: view.sourceIPs[srcVal] || "" } } });
      done(r, srcVal === "None" ? `Output ${cardIdx + 1}: disconnected` : `Output ${cardIdx + 1}: connecting to ${srcVal}\u2026`);
    }
  };
  var saveVideoSettings = async function(cardIdx) {
    const out = liveView2().outputs[cardIdx];
    const outputIndex = out?.output_index ?? cardIdx;
    const msg = document.getElementById(`out-msg-${cardIdx}`);
    if (msg) msg.textContent = "Applying\u2026";
    const whSel = document.getElementById(`res-wh-${cardIdx}`);
    const fpsSel = document.getElementById(`res-fps-${cardIdx}`);
    const whOpt = whSel?.options[whSel.selectedIndex];
    const fpsOpt = fpsSel?.options[fpsSel.selectedIndex];
    const results = [];
    let revision = await nativeRevision("video:" + outputIndex);
    let video = { ...(await nativeState()).outputs[outputIndex + 1]?.video ?? { resolution: "auto", scale_mode: "letterbox", rotation: 0 } };
    const saveVideo = async (next) => {
      const result = await nativeCommand({ set_output_video: { output: outputIndex + 1, video: next } }, revision);
      if (result.ok && result.revision !== void 0) {
        video = next;
        revision = result.revision;
        acceptNativeRevision("video:" + outputIndex, revision);
      }
      return result;
    };
    if (whOpt?.dataset.auto === "1") {
      results.push(await saveVideo({ ...video, resolution: "auto" }));
    } else if (whOpt?.dataset.w && fpsOpt) {
      const hz = parseFloat(fpsOpt.dataset.hz || fpsOpt.value);
      results.push(await saveVideo({ ...video, resolution: { mode: { width: +whOpt.dataset.w, height: +whOpt.dataset.h, refresh_hz: hz } } }));
    }
    const scale = document.getElementById(`scale-sel-${cardIdx}`)?.value || "letterbox";
    results.push(await saveVideo({ ...video, scale_mode: scale }));
    const rotation = parseInt(document.getElementById(`rotation-sel-${cardIdx}`)?.value || "0", 10);
    results.push(await saveVideo({ ...video, rotation }));
    const bad = results.find((r) => r && r.ok === false);
    if (msg) msg.textContent = bad ? "Failed" : "Saved";
    done(bad || {}, `Output ${cardIdx + 1}: video settings saved`);
  };
  expose("loadInitialData", () => loadInitialData2, (value) => {
    loadInitialData2 = value;
  });
  expose("loadVersionTag", () => loadVersionTag, (value) => {
    loadVersionTag = value;
  });
  expose("loadSourceIPs", () => loadSourceIPs2, (value) => {
    loadSourceIPs2 = value;
  });
  expose("fmtHz", () => fmtHz2, (value) => {
    fmtHz2 = value;
  });
  expose("outputModes", () => outputModes);
  expose("loadOutputModes", () => loadOutputModes, (value) => {
    loadOutputModes = value;
  });
  expose("populateOutputRefreshRates", () => populateOutputRefreshRates, (value) => {
    populateOutputRefreshRates = value;
  });
  expose("onResChange", () => onResChange, (value) => {
    onResChange = value;
  });
  expose("updateNdiPreview", () => updateNdiPreview, (value) => {
    updateNdiPreview = value;
  });
  expose("outputExpansionState", () => outputExpansionState);
  expose("outputExpansionKey", () => outputExpansionKey, (value) => {
    outputExpansionKey = value;
  });
  expose("outputExpansionOpen", () => outputExpansionOpen, (value) => {
    outputExpansionOpen = value;
  });
  expose("rememberOutputExpansion", () => rememberOutputExpansion, (value) => {
    rememberOutputExpansion = value;
  });
  expose("outputAudioDrafts", () => outputAudioDrafts);
  expose("outputAudioCatalog", () => outputAudioCatalog, (value) => {
    outputAudioCatalog = value;
  });
  expose("outputAudioEngine", () => outputAudioEngine, (value) => {
    outputAudioEngine = value;
  });
  expose("outputAudioCatalogPending", () => outputAudioCatalogPending, (value) => {
    outputAudioCatalogPending = value;
  });
  expose("outputAudioChannel", () => outputAudioChannel, (value) => {
    outputAudioChannel = value;
  });
  expose("outputAudioError", () => outputAudioError, (value) => {
    outputAudioError = value;
  });
  expose("loadOutputAudioCatalog", () => loadOutputAudioCatalog, (value) => {
    loadOutputAudioCatalog = value;
  });
  expose("outputAudioDraft", () => outputAudioDraft, (value) => {
    outputAudioDraft = value;
  });
  expose("validateOutputAudio", () => validateOutputAudio, (value) => {
    validateOutputAudio = value;
  });
  expose("outputAudioPayload", () => outputAudioPayload, (value) => {
    outputAudioPayload = value;
  });
  expose("mountOutputAudio", () => mountOutputAudio, (value) => {
    mountOutputAudio = value;
  });
  expose("renderOutputAudio", () => renderOutputAudio, (value) => {
    renderOutputAudio = value;
  });
  expose("refreshAudioFromServer", () => refreshAudioFromServer2, (value) => {
    refreshAudioFromServer2 = value;
  });
  expose("saveOutputAudio", () => saveOutputAudio, (value) => {
    saveOutputAudio = value;
  });
  expose("builtOutputCount", () => builtOutputCount, (value) => {
    builtOutputCount = value;
  });
  expose("builtOutputKeys", () => builtOutputKeys, (value) => {
    builtOutputKeys = value;
  });
  expose("buildOutputCards", () => buildOutputCards, (value) => {
    buildOutputCards = value;
  });
  expose("updateOutputCards", () => updateOutputCards2, (value) => {
    updateOutputCards2 = value;
  });
  expose("_lastSrcKey", () => _lastSrcKey, (value) => {
    _lastSrcKey = value;
  });
  expose("populateOutputSrcSelector", () => populateOutputSrcSelector, (value) => {
    populateOutputSrcSelector = value;
  });
  expose("onSrcChange", () => onSrcChange2, (value) => {
    onSrcChange2 = value;
  });
  expose("saveOutputName", () => saveOutputName, (value) => {
    saveOutputName = value;
  });
  expose("applySource", () => applySource, (value) => {
    applySource = value;
  });
  expose("saveVideoSettings", () => saveVideoSettings, (value) => {
    saveVideoSettings = value;
  });

  // src/ui-update.ts
  var updateUI2 = function() {
    updateTopbar();
    updateDash2();
    updateNDIState();
    renderTemps("dash-temps");
    renderTemps("sys-temps");
    const view = liveView2();
    setText("sys-up", view.uptime || "\u2014");
    setText("sys-plat", view.platform || "\u2014");
    updateOutputCards();
  };
  var liveSummary = function() {
    const outs = liveView2().outputs || [];
    const live = outs.filter((o) => o.connected);
    return { outs, live, sat: outs.some((o) => o.decode_saturated) };
  };
  var updateTopbar = function() {
    const { outs, live, sat } = liveSummary();
    const view = liveView2();
    const anyLive = live.length > 0 || view.connected;
    const pill = document.getElementById("topbar-pill");
    pill.className = "status-pill " + (sat ? "warn" : anyLive ? "live" : "idle");
    const pillText = outs.length > 1 ? anyLive ? `${live.length}/${outs.length} live` : "Idle" : anyLive ? "Live" : "Idle";
    pill.innerHTML = `<div class="dot"></div><span>${esc(pillText)}</span>`;
    if (outs.length > 1) {
      setText("topbar-src-label", "Sources");
      setText("topbar-src", live.length ? live.map((o) => o.source_name).join(" \xB7 ") : "\u2014");
    } else {
      setText("topbar-src-label", "Source");
      setText("topbar-src", view.source_name || "\u2014");
    }
    const res = view.display_width ? `${view.display_width}\xD7${view.display_height} @ ${fmtHz(view.display_refresh_hz || view.display_refresh)}` : "\u2014";
    const resEl = document.querySelector("#topbar-res span");
    if (resEl) resEl.textContent = res;
    setText("topbar-device", view.device_name || "\u2014");
    const nc = document.getElementById("nav-live-count");
    if (nc) {
      nc.style.display = live.length ? "" : "none";
      nc.textContent = live.length;
    }
  };
  var decodeChips2 = function(out) {
    if (!out.connected) return "";
    const chips = [];
    if (out.decode_backend && out.decode_backend !== "none") {
      chips.push(`<span class="chip ${out.hw_decode ? "chip-blue" : "chip-muted"}" title="${out.hw_decode ? "Hardware" : "Software"} decode (${esc(out.decode_backend)})">${out.hw_decode ? "HW" : "SW"} ${esc(out.decode_backend)}</span>`);
    }
    if (out.decode_saturated) {
      chips.push(`<span class="chip chip-warn" title="Decoder can't keep up with the source">SATURATED${out.decode_fps ? " " + Math.round(out.decode_fps) + " fps" : ""}</span>`);
    }
    return chips.join("");
  };
  var outputLabel = function(out, idx) {
    return out.output_alias || out.connector?.replace("HDMI-A-", "HDMI-") || `Output ${idx + 1}`;
  };
  var _lastTilesKey = "";
  var updateDash2 = function() {
    const tiles = document.getElementById("dash-outputs");
    const view = liveView2();
    const outs = view.outputs || [];
    if (tiles) {
      const key = JSON.stringify(outs.map((o) => [
        o.connected,
        o.source_name,
        o.input_width,
        o.input_height,
        o.width,
        o.height,
        o.refresh_hz || o.refresh,
        o.codec,
        o.decode_backend,
        o.hw_decode,
        o.decode_saturated,
        o.output_alias
      ]));
      outs.forEach((o, idx) => setText(`tile-fps-${idx}`, o.fps > 0 ? o.fps.toFixed(1) + " fps" : "\u2014"));
      if (key !== _lastTilesKey) {
        _lastTilesKey = key;
        tiles.innerHTML = outs.length ? outs.map((o, idx) => {
          const state = o.decode_saturated ? "sat" : o.connected ? "live" : "idle";
          const inRes = o.input_width ? `${o.input_width}\xD7${o.input_height}` : "\u2014";
          const outRes = o.width ? `${o.width}\xD7${o.height}` : "\u2014";
          const hz = o.width ? fmtHz(o.refresh_hz || o.refresh) : "";
          return `<div class="tile ${state}">
          <div class="tile-head">
            <span class="tile-name"><i class="bi bi-display"></i>${esc(outputLabel(o, idx))}</span>
            <span class="chip ${o.connected ? "chip-live" : "chip-idle"}">${o.connected ? "LIVE" : "IDLE"}</span>
          </div>
          <div class="tile-src" title="${esc(o.source_name || "")}">${o.connected || o.source_name ? esc(o.source_name) : "No source"}</div>
          <div class="tile-grid">
            <div><div class="k">Input</div><div class="v">${inRes}</div></div>
            <div><div class="k">Output</div><div class="v" title="${esc(hz)}">${outRes}</div></div>
            <div><div class="k">Frame rate</div><div class="v" id="tile-fps-${idx}">${o.fps > 0 ? o.fps.toFixed(1) + " fps" : "\u2014"}</div></div>
          </div>
          <div class="tile-chips">
            ${o.connected ? `<span class="chip chip-muted">${esc(fmtCodec(o.codec))}</span>` : ""}
            ${hz ? `<span class="chip chip-muted">${esc(hz)}</span>` : ""}
            ${decodeChips2(o)}
          </div>
        </div>`;
        }).join("") : '<div class="tile idle"><div class="tile-head"><span class="tile-name"><i class="bi bi-display"></i>Output</span><span class="chip chip-idle">\u2014</span></div><div class="tile-src">Waiting for the decoder\u2026</div></div>';
      }
    }
    setText("dash-plat", view.platform || "\u2014");
    const dp = view.total_frames > 0 ? (view.dropped_frames / view.total_frames * 100).toFixed(1) : "0.0";
    setText("dash-frames", view.total_frames ? `${view.total_frames.toLocaleString()} / ${view.dropped_frames.toLocaleString()} (${dp}%)` : "\u2014");
    const cpu = view.cpu_percent || 0, mem = (view.memory || {}).percent || 0;
    setText("dash-cpu-pct", cpu + "%");
    setBar("dash-cpu-bar", cpu);
    setText("dash-mem-pct", mem + "% \xB7 " + (view.memory.used_mb || 0) + " / " + (view.memory.total_mb || 0) + " MB");
    setBar("dash-mem-bar", mem, true);
    setText("dash-uptime", view.uptime || "\u2014");
    setText("dash-audio", view.audio_enabled ? "Enabled" : "Disabled");
    const satWarn = document.getElementById("sat-warning");
    if (satWarn) satWarn.style.display = outs.some((o) => o.decode_saturated) ? "" : "none";
    setText("sys-cpu-pct", cpu + "%");
    setBar("sys-cpu-bar", cpu);
    setText("sys-mem-pct", mem + "%");
    setBar("sys-mem-bar", mem, true);
    const wrap = document.getElementById("dash-target-wrap");
    const sel = document.getElementById("dash-target-out");
    if (wrap && sel) {
      wrap.style.display = outs.length > 1 ? "" : "none";
      const html = outputOptionsHtml();
      if (sel.dataset.html !== html) {
        const prev = sel.value;
        sel.innerHTML = html;
        sel.dataset.html = html;
        if (prev) sel.value = prev;
      }
    }
    setText("dash-src-count", view.sources.length);
    const sl = document.getElementById("dash-src-list");
    if (sl) {
      const liveBy = {};
      outs.forEach((o, idx) => {
        if (o.connected && o.source_name) (liveBy[o.source_name] ||= []).push(outputLabel(o, idx));
      });
      const srcKey = JSON.stringify([view.sources, liveBy, view.sourceIPs]);
      if (sl.dataset.key !== srcKey) {
        sl.dataset.key = srcKey;
        sl.className = "";
        if (!view.sources.length) {
          sl.innerHTML = '<div class="empty"><i class="bi bi-wifi-off"></i>No NDI sources found. Check the network, groups, or discovery server.</div>';
        } else {
          sl.innerHTML = view.sources.map((s) => {
            const on = liveBy[s];
            return `<div class="source-item ${on ? "active-src" : ""}">
            <span class="src-dot ${on ? "on" : ""}"></span>
            <span class="src-name" title="${esc(s)}">${esc(s)}${view.sourceIPs[s] ? ` <span class="src-ip">${esc(view.sourceIPs[s])}</span>` : ""}</span>
            ${on ? `<span class="src-tag">ON ${esc(on.join(", ").toUpperCase())}</span>` : ""}
            <button class="btn btn-outline-primary btn-sm" style="padding:2px 10px;font-size:.74rem" onclick="connectFromDash('${esc(s, 1)}')"><i class="bi bi-plug me-1"></i>Connect</button>
          </div>`;
          }).join("");
        }
        const filter = document.getElementById("dash-src-filter");
        if (filter) filter.style.display = view.sources.length > 8 ? "" : "none";
        applySourceFilter();
      }
    }
  };
  var applySourceFilter = function() {
    const q = (document.getElementById("dash-src-filter")?.value || "").trim().toLowerCase();
    document.querySelectorAll("#dash-src-list .source-item").forEach((item) => {
      const name = item.querySelector(".src-name")?.textContent.toLowerCase() || "";
      item.style.display = !q || name.includes(q) ? "" : "none";
    });
  };
  expose("updateUI", () => updateUI2, (value) => {
    updateUI2 = value;
  });
  expose("liveSummary", () => liveSummary, (value) => {
    liveSummary = value;
  });
  expose("updateTopbar", () => updateTopbar, (value) => {
    updateTopbar = value;
  });
  expose("decodeChips", () => decodeChips2, (value) => {
    decodeChips2 = value;
  });
  expose("outputLabel", () => outputLabel, (value) => {
    outputLabel = value;
  });
  expose("_lastTilesKey", () => _lastTilesKey, (value) => {
    _lastTilesKey = value;
  });
  expose("updateDash", () => updateDash2, (value) => {
    updateDash2 = value;
  });
  expose("applySourceFilter", () => applySourceFilter, (value) => {
    applySourceFilter = value;
  });

  // src/unsaved-changes.ts
  var markDirty2 = function(el) {
    el?.closest?.(".content .card")?.classList.add("dirty");
  };
  document.addEventListener("DOMContentLoaded", () => {
    const selfSaving = (t) => /save|apply/i.test(t.getAttribute?.("onchange") || "");
    const onEdit = (e) => {
      const card = e.target.closest?.(".content .card");
      if (card && card.querySelector(".btn-primary") && !selfSaving(e.target)) card.classList.add("dirty");
    };
    document.addEventListener("input", onEdit);
    document.addEventListener("change", onEdit);
    document.addEventListener("click", (e) => {
      if (e.target.closest?.(".btn-primary, .btn-outline-primary")) e.target.closest(".content .card")?.classList.remove("dirty");
    });
  });
  var updateNDIState2 = function() {
    const el = document.getElementById("ndi-connections");
    const view = liveView2();
    if (el && view.outputs?.length) {
      el.innerHTML = view.outputs.map((out, idx) => {
        const badge = out.connected ? '<span class="chip chip-live">LIVE</span>' : '<span class="chip chip-idle">IDLE</span>';
        const label = out.output_alias || `Output ${idx + 1}`;
        const src = out.source_name || "None";
        const res = out.input_width ? `${out.input_width}\xD7${out.input_height}` : "\u2014";
        const fps = out.fps > 0 ? out.fps.toFixed(1) + " fps" : "\u2014";
        const codec = fmtCodec(out.codec);
        const dec = decodeChips(out);
        return `<div class="col-12 col-md-6 connection-summary">
        <div class="stat-row"><span class="sub-head mb-0"><i class="bi bi-display"></i>${esc(label)}</span><span class="d-flex gap-1 align-items-center">${dec}${badge}</span></div>
        <div class="stat-row"><span class="stat-label">Source</span><span class="stat-val" title="${esc(src)}">${esc(src)}</span></div>
        <div class="stat-row"><span class="stat-label">Input</span><span class="stat-val">${res}</span></div>
        <div class="stat-row"><span class="stat-label">Frame rate \xB7 Codec</span><span class="stat-val">${fps} \xB7 ${codec}</span></div>
      </div>`;
      }).join("");
    } else if (el) {
      el.innerHTML = '<div class="empty"><i class="bi bi-hourglass-split"></i>Waiting for the decoder\u2026</div>';
    }
    if (view.decoder) {
      const tally = view.decoder.tally === "TallyOn" ? "TallyOn" : "TallyOff";
      setVal("tally-sel", tally);
      setVal("ndi-audio-sel", view.decoder.audio);
    }
    if (view.outputs?.length) {
      const backends = [...new Set(view.outputs.filter((o) => o.connected && o.decode_backend && o.decode_backend !== "none").map((o) => (o.hw_decode ? "HW " : "SW ") + o.decode_backend))];
      setText("decode-backend-hint", "Active backend: " + (backends.length ? backends.join(", ") : "\u2014"));
    }
  };
  var renderTemps2 = function(id) {
    const el = document.getElementById(id);
    if (!el) return;
    const t = liveView2().temps || {}, keys = Object.keys(t);
    if (!keys.length) {
      el.innerHTML = '<div class="form-text" style="grid-column:1/-1">No temperature sensors</div>';
      return;
    }
    el.innerHTML = keys.map((k) => {
      const v = t[k], cls = v > 75 ? "temp-hot" : v > 60 ? "temp-warm" : "temp-ok";
      const lbl = k.replace("thermal", "").replace(/_/g, " ").trim() || k;
      return `<div class="temp-chip"><div class="temp-val ${cls}">${v.toFixed(0)}\xB0C</div><div class="temp-lbl">${esc(lbl)}</div></div>`;
    }).join("");
  };
  expose("markDirty", () => markDirty2, (value) => {
    markDirty2 = value;
  });
  expose("updateNDIState", () => updateNDIState2, (value) => {
    updateNDIState2 = value;
  });
  expose("renderTemps", () => renderTemps2, (value) => {
    renderTemps2 = value;
  });

  // src/settings.ts
  var loadSettings2 = async function() {
    try {
      await loadNativeSettings();
    } catch {
    }
    loadSplashSettings2();
    loadOsd();
    loadNetwork();
  };
  var loadNtp2 = async function() {
    try {
      const state = await nativeState();
      setVal("ntp-server", state.device.ntp_server || "");
      acceptNativeRevision("ntp", state.revision);
      const n = await api("/v1/DeviceSettings/ntp");
      const sync = n.synchronized ? "synchronized" : "not synchronized";
      const srv = n.active_server ? ` with ${n.active_server}` : "";
      setText("ntp-status", `OS clock ${sync}${srv}. Persists across reboots.`);
    } catch {
    }
  };
  var saveNtp = async function() {
    const ntp = document.getElementById("ntp-server").value.trim();
    const r = await nativeCommand({ set_device: devicePatch({ ntp_server: ntp }) }, await nativeRevision("ntp"));
    if (r.ok) acceptNativeRevision("ntp", r.revision);
    done(r, ntp ? `NTP server set to ${ntp}` : "NTP server reset to OS default");
    setTimeout(loadNtp2, 1500);
  };
  var logoPath = (logo) => logo?.media_type === "image/png" ? "/etc/ndi-splash-logo.png" : logo?.media_type === "image/jpeg" ? "/etc/ndi-splash-logo.jpg" : "";
  var loadSplashSettings2 = async function() {
    try {
      const state = await nativeState();
      const d = state.display.splash;
      setColorField("sp-bg-idle", "sp-bg-idle-hex", d.bg_idle);
      setColorField("sp-bg-live", "sp-bg-live-hex", d.bg_live);
      setColorField("sp-ac-idle", "sp-ac-idle-hex", d.accent_idle);
      setColorField("sp-ac-live", "sp-ac-live-hex", d.accent_live);
      setVal("sp-logo-path", logoPath(d.logo));
      setSlider("sp-logo-x", "sp-logo-x-val", d.logo_x_pct, "%");
      setSlider("sp-logo-y", "sp-logo-y-val", d.logo_y_pct, "%");
      setSlider("sp-logo-w", "sp-logo-w-val", d.logo_w_pct, "%");
      setVal("sp-text-idle", d.text_idle || "");
      setVal("sp-text-live", d.text_live || "");
      setSlider("sp-text-height", "sp-text-h-val", d.text_height_pct || 10, "%");
      setCheck("sp-show-signal", d.show_signal_text !== false);
      setCheck("sp-show-devname", d.show_device_name !== false);
      setCheck("sp-show-devurl", d.show_device_url !== false);
      setCheck("sp-show-sources", d.show_sources_available !== false);
      setBackdrop(d.backdrop || "gradient", true);
      acceptNativeRevision("splash", state.revision);
    } catch (e) {
      console.warn("splash config load failed", e);
    }
    updateSplashPreview2();
  };
  var _spMode = "idle";
  var setSplashPreviewMode = function(mode) {
    _spMode = mode;
    document.querySelectorAll("#sp-pv-mode button").forEach((b) => b.classList.toggle("active", b.dataset.mode === mode));
    updateSplashPreview2();
  };
  var updateSplashPreview2 = function() {
    const pv = document.getElementById("sp-preview");
    if (!pv) return;
    const v = (id) => document.getElementById(id)?.value;
    const on = (id) => !!document.getElementById(id)?.checked;
    const live = _spMode === "live";
    pv.style.setProperty("--sp-bg", v(live ? "sp-bg-live" : "sp-bg-idle") || "#000");
    pv.style.setProperty("--sp-ac", v(live ? "sp-ac-live" : "sp-ac-idle") || "#fff");
    pv.classList.toggle("flat", v("sp-backdrop") === "flat");
    const h = pv.clientHeight || 200;
    const textPct = +(v("sp-text-height") || 4);
    const statusPx = Math.max(8, h * textPct / 100);
    const subPx = Math.max(6, statusPx * 0.6);
    const pill = document.getElementById("sp-pv-pill");
    setText("sp-pv-text", live ? v("sp-text-live") || "Signal Available" : v("sp-text-idle") || "No Signal");
    pill.style.fontSize = statusPx + "px";
    pill.style.display = on("sp-show-signal") ? "" : "none";
    const src = document.getElementById("sp-pv-src");
    src.style.fontSize = subPx + "px";
    src.style.display = on("sp-show-sources") ? "" : "none";
    const n = liveView2().sources.length;
    setText("sp-pv-src", n === 0 ? "No sources found" : `${n} source${n === 1 ? "" : "s"} available`);
    const dev = document.getElementById("sp-pv-dev");
    setText("sp-pv-dev", document.getElementById("ndi-alias")?.value.trim() || liveView2().device_name || "NDIMON-XXXX");
    dev.style.fontSize = statusPx * 0.8 + "px";
    dev.style.display = on("sp-show-devname") ? "" : "none";
    const url = document.getElementById("sp-pv-url");
    setText("sp-pv-url", location.origin);
    url.style.fontSize = subPx + "px";
    url.style.display = on("sp-show-devurl") ? "" : "none";
    const logo = document.getElementById("sp-pv-logo");
    logo.style.left = v("sp-logo-x") + "%";
    logo.style.top = v("sp-logo-y") + "%";
    logo.style.width = v("sp-logo-w") + "%";
    logo.style.display = v("sp-logo-path") ? "" : "none";
  };
  var setBackdrop = function(value, silent) {
    const v = value === "flat" ? "flat" : "gradient";
    const input = document.getElementById("sp-backdrop");
    if (!input) return;
    input.value = v;
    document.querySelectorAll("#sp-backdrop-seg button").forEach((b) => b.classList.toggle("active", b.dataset.v === v));
    updateSplashPreview2();
    if (!silent) markDirty(input);
  };
  var SPLASH_PALETTES = [
    { name: "Midnight", idle: ["#0D1B2A", "#4488CC"], live: ["#0D2B1A", "#22FF88"] },
    { name: "Graphite", idle: ["#14171C", "#9AA4B2"], live: ["#121A16", "#3DDC84"] },
    { name: "Slate", idle: ["#0F172A", "#60A5FA"], live: ["#052E16", "#4ADE80"] },
    { name: "Studio", idle: ["#050505", "#F59E0B"], live: ["#050505", "#22C55E"] }
  ];
  var renderSplashPalettes = function() {
    const el = document.getElementById("sp-palettes");
    if (!el) return;
    el.innerHTML = SPLASH_PALETTES.map((p, i) => `<button type="button" class="sp-swatch" onclick="applySplashPalette(${i})" title="${p.name}: fills the four colours">` + [...p.idle, ...p.live].map((c) => `<span style="background:${c}"></span>`).join("") + `<b>${p.name}</b></button>`).join("");
  };
  var applySplashPalette = function(i) {
    const p = SPLASH_PALETTES[i];
    if (!p) return;
    setColorField("sp-bg-idle", "sp-bg-idle-hex", p.idle[0]);
    setColorField("sp-ac-idle", "sp-ac-idle-hex", p.idle[1]);
    setColorField("sp-bg-live", "sp-bg-live-hex", p.live[0]);
    setColorField("sp-ac-live", "sp-ac-live-hex", p.live[1]);
    updateSplashPreview2();
    markDirty(document.getElementById("sp-palettes"));
  };
  document.addEventListener("DOMContentLoaded", renderSplashPalettes);
  document.addEventListener("DOMContentLoaded", () => {
    const card = document.getElementById("sp-preview")?.closest(".card");
    if (card) card.addEventListener("input", updateSplashPreview2);
    window.addEventListener("resize", updateSplashPreview2);
  });
  var setColorField = function(pickerId, hexId, val) {
    if (!val) return;
    const picker = document.getElementById(pickerId);
    const hex = document.getElementById(hexId);
    if (picker) picker.value = val;
    if (hex) hex.value = val;
  };
  var setSlider = function(sliderId, labelId, val, suffix) {
    const sl = document.getElementById(sliderId);
    const lb = document.getElementById(labelId);
    if (sl) sl.value = Math.round(val);
    if (lb) lb.textContent = Math.round(val) + (suffix || "");
  };
  var syncColorHex = function(pickerId, hexId) {
    const hex = document.getElementById(hexId);
    const picker = document.getElementById(pickerId);
    if (!hex || !picker) return;
    const v = hex.value.trim();
    if (/^#[0-9a-fA-F]{6}$/.test(v)) {
      picker.value = v;
      updateSplashPreview2();
    }
  };
  document.addEventListener("DOMContentLoaded", () => {
    const pairs = [
      ["sp-bg-idle", "sp-bg-idle-hex"],
      ["sp-bg-live", "sp-bg-live-hex"],
      ["sp-ac-idle", "sp-ac-idle-hex"],
      ["sp-ac-live", "sp-ac-live-hex"]
    ];
    for (const [pid, hid] of pairs) {
      const p = document.getElementById(pid);
      if (p) p.addEventListener("input", () => {
        const h = document.getElementById(hid);
        if (h) h.value = p.value;
        updateSplashPreview2();
      });
    }
  });
  var loadSysInfo2 = async function() {
    try {
      const d = await api("/v1/AboutMe");
      setText("sys-model", d.Model || "\u2014");
      setText("sys-host", d.HostName || d.DeviceName || "\u2014");
      setText("sys-ip", Array.isArray(d.IPAddresses) ? d.IPAddresses[0] : d.IPAddress || "\u2014");
      setText("sys-ndi", d.NDIVersion || "\u2014");
      setText("sys-fw", d.FirmwareVersion || "\u2014");
      setText("sys-build", d.BuildDate || "\u2014");
    } catch {
    }
    loadServices();
    loadUpdateStatus();
    loadVaapiInfo();
  };
  var loadVaapiInfo = async function() {
    try {
      const v = await api("/api/v1/system/vaapi-info");
      const row = document.getElementById("sys-vaapi-row");
      if (!v.available) {
        if (row) row.style.display = "none";
        return;
      }
      if (row) row.style.display = "";
      const profs = (v.decode_profiles || []).join(", ") || "none";
      setText("sys-vaapi", `${v.driver || "VAAPI"} \u2014 ${profs}`);
    } catch {
    }
  };
  expose("loadSettings", () => loadSettings2, (value) => {
    loadSettings2 = value;
  });
  expose("loadNtp", () => loadNtp2, (value) => {
    loadNtp2 = value;
  });
  expose("saveNtp", () => saveNtp, (value) => {
    saveNtp = value;
  });
  expose("loadSplashSettings", () => loadSplashSettings2, (value) => {
    loadSplashSettings2 = value;
  });
  expose("_spMode", () => _spMode, (value) => {
    _spMode = value;
  });
  expose("setSplashPreviewMode", () => setSplashPreviewMode, (value) => {
    setSplashPreviewMode = value;
  });
  expose("updateSplashPreview", () => updateSplashPreview2, (value) => {
    updateSplashPreview2 = value;
  });
  expose("setBackdrop", () => setBackdrop, (value) => {
    setBackdrop = value;
  });
  expose("SPLASH_PALETTES", () => SPLASH_PALETTES);
  expose("renderSplashPalettes", () => renderSplashPalettes, (value) => {
    renderSplashPalettes = value;
  });
  expose("applySplashPalette", () => applySplashPalette, (value) => {
    applySplashPalette = value;
  });
  expose("setColorField", () => setColorField, (value) => {
    setColorField = value;
  });
  expose("setSlider", () => setSlider, (value) => {
    setSlider = value;
  });
  expose("syncColorHex", () => syncColorHex, (value) => {
    syncColorHex = value;
  });
  expose("loadSysInfo", () => loadSysInfo2, (value) => {
    loadSysInfo2 = value;
  });
  expose("loadVaapiInfo", () => loadVaapiInfo, (value) => {
    loadVaapiInfo = value;
  });

  // src/software-update.ts
  var signedReleaseRequired = "updates need a signed release; no release key is configured";
  var loadUpdateStatus2 = async function() {
    try {
      const v = await api("/api/v1/system/version");
      const el = document.getElementById("upd-status");
      const btn = document.getElementById("upd-apply-btn");
      let txt = v.commit ? `commit ${String(v.commit).slice(0, 9)}` : v.firmware || "\u2014";
      if (v.update_running) txt = "Updating\u2026";
      else if (!v.update_supported) txt = v.update_message || `${txt} \xB7 manual updates only`;
      setText("upd-status", txt);
      if (btn) btn.disabled = !v.update_supported;
    } catch {
    }
  };
  var checkUpdate = async function() {
    setText("upd-status", "Checking\u2026");
    try {
      const v = await api("/api/v1/system/version");
      const btn = document.getElementById("upd-apply-btn");
      if (!v.update_supported) {
        setText("upd-status", v.update_message || signedReleaseRequired);
        if (btn) btn.disabled = true;
      } else if (v.updates_available > 0) {
        setText("upd-status", `${v.updates_available} update(s) available`);
        if (btn) btn.disabled = false;
      } else if (v.updates_available === 0) {
        setText("upd-status", "Up to date");
        if (btn) btn.disabled = true;
      } else {
        setText("upd-status", v.update_message || "A signed release version is required");
      }
    } catch {
      setText("upd-status", "Check failed");
    }
  };
  var applyUpdate = async function() {
    try {
      const version = await api("/api/v1/system/version");
      if (!version.update_supported) {
        setText("upd-status", version.update_message || signedReleaseRequired);
        toast(version.update_message || signedReleaseRequired, "warning");
        return;
      }
      const target = version.latest_version;
      if (!target) {
        setText("upd-status", "No signed release is available to install");
        return;
      }
      if (!await confirmDialog("Update now?", "Install a signed release and restart services. The web UI will be unavailable during the update.", { label: "Update now" })) return;
      toast("Starting update\u2026", "warning");
      const r = await confirmedCommand({ install_update: { package: null, version: target, force: false } }, { title: "Update now?", label: "Update now" });
      if (r.ok) setText("upd-status", "Updating\u2026 services will restart");
      else toast("Update failed: " + (r.error || "unknown"), "danger");
    } catch {
      toast("Update request failed", "danger");
    }
  };
  expose("loadUpdateStatus", () => loadUpdateStatus2, (value) => {
    loadUpdateStatus2 = value;
  });
  expose("checkUpdate", () => checkUpdate, (value) => {
    checkUpdate = value;
  });
  expose("applyUpdate", () => applyUpdate, (value) => {
    applyUpdate = value;
  });

  // src/watchdog.ts
  var loadWatchdog2 = async function() {
    try {
      const state = await nativeState();
      setVal("wd-mode", state.device.watchdog_mode || "active");
      acceptNativeRevision("watchdog", state.revision);
    } catch {
    }
    refreshWatchdogStats2();
  };
  var saveWatchdogMode = async function() {
    const mode = document.getElementById("wd-mode").value;
    const r = await nativeCommand({ set_device: devicePatch({ watchdog_mode: mode }) }, await nativeRevision("watchdog"));
    if (r.ok) acceptNativeRevision("watchdog", r.revision);
    done(r, "Watchdog mode: " + mode);
    setTimeout(refreshWatchdogStats2, 500);
  };
  var refreshWatchdogStats2 = async function() {
    try {
      const h = await api("/api/health");
      let health = "ok", stall = 0;
      if (h.outputs) for (const o of h.outputs) {
        if (o.health !== "ok" && o.health !== "idle") health = o.health;
        stall = Math.max(stall, o.stall_count || 0);
      }
      const hColor = health === "ok" ? "var(--live)" : health === "degraded" ? "var(--warn)" : "var(--danger)";
      setText("wd-health", health);
      const hEl = document.getElementById("wd-health");
      if (hEl) hEl.style.color = hColor;
      setText("wd-stall", stall);
      const w = await api("/api/watchdog-stats");
      const badge = document.getElementById("wd-badge");
      const mode = w.watchdog_mode || "unknown";
      if (badge) {
        const colors = { disabled: "chip-muted", passive: "chip-blue", active: "chip-live", unknown: "chip-warn" };
        badge.className = "chip " + (colors[mode] || "chip-muted");
        badge.textContent = mode.charAt(0).toUpperCase() + mode.slice(1);
      }
      setText("wd-recv", w.recv_stalls || 0);
      setText("wd-video", w.video_stalls || 0);
      setText("wd-dec-stall", w.decoder_stalls || 0);
      setText("wd-disp-freeze", w.display_freezes || 0);
      setText("wd-reconnect", w.reconnects || 0);
      setText("wd-svc-restart", w.service_restarts || 0);
    } catch {
    }
  };
  expose("loadWatchdog", () => loadWatchdog2, (value) => {
    loadWatchdog2 = value;
  });
  expose("saveWatchdogMode", () => saveWatchdogMode, (value) => {
    saveWatchdogMode = value;
  });
  expose("refreshWatchdogStats", () => refreshWatchdogStats2, (value) => {
    refreshWatchdogStats2 = value;
  });

  // src/presets.ts
  var _presets = [];
  var loadPresets2 = async function() {
    try {
      const state = await nativeState();
      _presets = Object.entries(state.presets).filter((entry) => !!entry[1]).sort((a, b) => (a[1].position ?? Number.MAX_SAFE_INTEGER) - (b[1].position ?? Number.MAX_SAFE_INTEGER)).map(([name, preset]) => ({ name, source: preset.source.name, ip: preset.source.endpoint }));
    } catch {
      _presets = [];
    }
    renderPresets();
    populatePresetSourceSelect2();
  };
  var populatePresetSourceSelect2 = function() {
    const sel = document.getElementById("preset-src");
    if (!sel) return;
    const prev = sel.value;
    sel.innerHTML = "";
    liveView2().sources.forEach((s) => {
      const o = document.createElement("option");
      o.value = s;
      o.textContent = s;
      sel.appendChild(o);
    });
    if (prev) sel.value = prev;
  };
  var outputOptionsHtml2 = function() {
    const outs = liveView2().outputs || [];
    if (!outs.length) return '<option value="1">Output 1</option>';
    return outs.map((o, i) => {
      const label = o.output_alias || o.connector || `Output ${i + 1}`;
      return `<option value="${(o.output_index ?? i) + 1}">${esc(label)}</option>`;
    }).join("");
  };
  var renderPresets = function() {
    const el = document.getElementById("preset-list");
    if (!el) return;
    if (!_presets.length) {
      el.innerHTML = '<div class="empty"><i class="bi bi-bookmark"></i>No presets saved.</div>';
      return;
    }
    const multi = (liveView2().outputs || []).length > 1;
    el.innerHTML = _presets.map((p, i) => `
    <div class="source-item" style="gap:.5rem">
      <span class="src-name" style="line-height:1.25">
        <strong>${esc(p.name)}</strong>
        <span class="form-text" style="display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;margin:0">${esc(p.source)}</span>
      </span>
      <select class="form-select form-select-sm" id="preset-out-${i}" style="width:auto;${multi ? "" : "display:none"}" aria-label="Output">${outputOptionsHtml2()}</select>
      <button class="btn btn-primary btn-sm" onclick="recallPreset(${i})" title="Recall"><i class="bi bi-play-fill"></i></button>
      <button class="btn btn-outline-danger btn-sm" onclick="deletePreset(${i})" title="Delete"><i class="bi bi-trash"></i></button>
    </div>`).join("");
  };
  var savePreset = async function() {
    const name = (document.getElementById("preset-name")?.value || "").trim();
    const source = document.getElementById("preset-src")?.value || "";
    if (!name) {
      toast("Enter a preset name", "warning");
      return;
    }
    if (!source) {
      toast("No source selected", "warning");
      return;
    }
    const r = await nativeCommand({ save_preset: { name, preset: { source: { name: source, endpoint: liveView2().sourceIPs[source] || "" } } } });
    if (r.ok) {
      document.getElementById("preset-name").value = "";
      await loadPresets2();
      toast("Preset saved");
    } else toast("Save failed: " + (r.error || "unknown"), "danger");
  };
  var recallPreset = async function(idx) {
    const p = _presets[idx];
    if (!p) return;
    const outSel = document.getElementById(`preset-out-${idx}`);
    const output = parseInt(outSel?.value || "1", 10);
    const r = await nativeCommand({ recall_preset: { name: p.name, output } });
    if (r.ok) toast(`Recalled "${p.name}" \u2192 output ${output}`);
    else toast("Recall failed: " + (r.error || "unknown"), "danger");
  };
  var deletePreset = async function(idx) {
    const p = _presets[idx];
    if (!p) return;
    if (!await confirmDialog("Delete preset", `Delete the preset "${p.name}"?`, { danger: true, label: "Delete" })) return;
    const r = await nativeCommand({ delete_preset: { name: p.name } });
    if (r.ok) {
      await loadPresets2();
      toast("Preset deleted");
    }
  };
  expose("_presets", () => _presets, (value) => {
    _presets = value;
  });
  expose("loadPresets", () => loadPresets2, (value) => {
    loadPresets2 = value;
  });
  expose("populatePresetSourceSelect", () => populatePresetSourceSelect2, (value) => {
    populatePresetSourceSelect2 = value;
  });
  expose("outputOptionsHtml", () => outputOptionsHtml2, (value) => {
    outputOptionsHtml2 = value;
  });
  expose("renderPresets", () => renderPresets, (value) => {
    renderPresets = value;
  });
  expose("savePreset", () => savePreset, (value) => {
    savePreset = value;
  });
  expose("recallPreset", () => recallPreset, (value) => {
    recallPreset = value;
  });
  expose("deletePreset", () => deletePreset, (value) => {
    deletePreset = value;
  });

  // src/actions.ts
  var doConnect = async function(name, ip, outputCh) {
    const r = await nativeCommand(!name || name === "None" ? { forget_source: { output: outputCh || 1 } } : { select_source: { output: outputCh || 1, source: { name, endpoint: ip || "" } } });
    done(r, "Connecting to " + name + "\u2026");
  };
  var connectFromDash = function(name) {
    const sel = document.getElementById("dash-target-out");
    const view = liveView2();
    const ch = (view.outputs || []).length > 1 && sel?.value ? parseInt(sel.value, 10) : 1;
    doConnect(name, view.sourceIPs[name] || "", ch);
  };
  var doDisconnect = async function(cardIdx) {
    const out = liveView2().outputs[cardIdx];
    const output = (out?.output_index ?? cardIdx ?? 0) + 1;
    const r = await nativeCommand({ forget_source: { output } });
    const sel = document.getElementById(`src-sel-${cardIdx || 0}`);
    if (sel) {
      sel.value = "None";
      onSrcChange(cardIdx || 0);
    }
    done(r, `Output ${(cardIdx || 0) + 1}: disconnected`);
  };
  var refreshSources = async function() {
    const r = await nativeCommand("refresh_ndi");
    if (r.cancelled || r.ok === false && r.error === "Signed out") return;
    toast("Rescanning NDI sources\u2026");
    setTimeout(loadSourceIPs, 3e3);
  };
  var saveTally = async function() {
    const state = await nativeState();
    const value = document.getElementById("tally-sel").value;
    const r = await nativeCommand({ set_ndi: { ...state.ndi, tally: value === "TallyOn" ? "on" : value === "TallyVideo" ? "video" : "off" } }, state.revision);
    done(r, "Tally saved");
  };
  var saveAudioMode = async function() {
    const state = await nativeState();
    const enabled = document.getElementById("ndi-audio-sel").value === "NDIAudioEn";
    const r = await nativeCommand({ put_output_settings: { outputs: {}, ndi: null, audio_enabled: { 1: enabled } } }, state.revision);
    done(r, "Audio saved");
  };
  var saveDecoder = async function() {
    const state = await nativeState();
    const receive = transportFromLegacy(document.getElementById("tp-sel").value);
    const decode_mode = document.getElementById("decode-mode-sel").value;
    const a = await nativeCommand({ set_ndi: { ...state.ndi, receive } }, await nativeRevision("decoder"));
    const b = await nativeCommand({ set_device: devicePatch({ decode_mode }) }, a.ok ? a.revision : await nativeRevision("decoder"));
    if (a.ok && b.ok) acceptNativeRevision("decoder", b.revision);
    done(a.ok === false ? a : b, "Decoder settings saved");
  };
  var saveSplash = async function() {
    const state = await nativeState();
    const field = (id) => document.getElementById(id);
    const current = state.display.splash;
    const splash = {
      ...current,
      bg_idle: field("sp-bg-idle").value,
      bg_live: field("sp-bg-live").value,
      accent_idle: field("sp-ac-idle").value,
      accent_live: field("sp-ac-live").value,
      logo: field("sp-logo-path").value.trim() ? current.logo : null,
      logo_x_pct: +field("sp-logo-x").value,
      logo_y_pct: +field("sp-logo-y").value,
      logo_w_pct: +field("sp-logo-w").value,
      text_idle: field("sp-text-idle").value,
      text_live: field("sp-text-live").value,
      text_height_pct: +field("sp-text-height").value,
      show_signal_text: field("sp-show-signal").checked,
      show_device_name: field("sp-show-devname").checked,
      show_device_url: field("sp-show-devurl").checked,
      show_sources_available: field("sp-show-sources").checked,
      backdrop: field("sp-backdrop").value
    };
    const r = await nativeCommand({ set_display: { splash, osd: state.display.osd } }, await nativeRevision("splash"));
    if (r.ok) acceptNativeRevision("splash", r.revision);
    return done(r, "Splash settings saved");
  };
  var previewSplash = async function(sourceAvailable) {
    if (!await saveSplash()) return;
    const r = await nativeCommand({ preview_splash: { source_available: sourceAvailable } });
    done(r, "Splash preview sent to the display");
  };
  var uploadLogo = async function(input) {
    const file = input.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = async (e) => {
      const dataUrl = e.target.result;
      const base64 = dataUrl.split(",")[1];
      try {
        const r = await post("/v1/Splash/uploadLogo", { data: base64, filename: file.name });
        if (r.ok) {
          document.getElementById("sp-logo-path").value = r.path;
          updateSplashPreview();
          try {
            await store.reload();
          } catch {
          }
          acceptNativeRevision("splash", store.state?.revision);
          toast("Logo uploaded. Save to apply it.");
        } else {
          toast("Upload failed: " + (r.error || "unknown"), "danger");
        }
      } catch (err) {
        toast("Upload error: " + err.message, "danger");
      }
    };
    reader.readAsDataURL(file);
    input.value = "";
  };
  var loadOsd2 = async function() {
    try {
      const state = await nativeState();
      document.getElementById("osd-enabled").checked = !!state.display.osd.enabled;
      document.getElementById("osd-text").value = state.display.osd.text || "";
      acceptNativeRevision("osd", state.revision);
    } catch {
    }
  };
  var saveOsd = async function() {
    const state = await nativeState();
    const enabled = document.getElementById("osd-enabled").checked;
    const text = document.getElementById("osd-text").value.trim();
    const r = await nativeCommand({ set_display: { splash: state.display.splash, osd: { enabled, text } } }, await nativeRevision("osd"));
    if (r.ok) acceptNativeRevision("osd", r.revision);
    done(r, "OSD settings saved");
  };
  var saveNetSettings = async function() {
    const state = await nativeState();
    const ip = document.getElementById("disc-ip").value.trim();
    const groups = getNdiGroupsString().split(",").map((value) => value.trim()).filter(Boolean);
    const ips = document.getElementById("off-ips").value.trim();
    const alias = document.getElementById("ndi-alias").value.trim();
    const split = (value) => value.split(/[\s,;]+/).filter(Boolean);
    const results = [];
    let ndi = { ...state.ndi };
    let revision = await nativeRevision("discovery");
    const save = async (command) => {
      const result = await nativeCommand(command, revision);
      if (result.ok && result.revision !== void 0) {
        revision = result.revision;
        if (typeof command === "object" && "set_ndi" in command) ndi = command.set_ndi;
      }
      results.push(result);
    };
    await save({ set_ndi: { ...ndi, discovery_servers: split(ip) } });
    await save({ set_ndi: { ...ndi, groups } });
    await save({ set_ndi: { ...ndi, off_subnet_sources: split(ips) } });
    const advertise = document.getElementById("ds-advertise");
    const control = document.getElementById("ds-control");
    if (advertise && control && (advertise.checked !== ndi.receiver_advertising || control.checked !== ndi.remote_control))
      await save({ set_ndi: { ...ndi, receiver_advertising: advertise.checked, remote_control: control.checked } });
    await save({ set_device: devicePatch({ ndi_alias: alias }) });
    const bad = results.find((r) => r && r.ok === false);
    if (!done(bad || {}, "Discovery settings saved")) return;
    acceptNativeRevision("discovery", revision);
    updateSplashPreview();
    refreshSources();
  };
  expose("doConnect", () => doConnect, (value) => {
    doConnect = value;
  });
  expose("connectFromDash", () => connectFromDash, (value) => {
    connectFromDash = value;
  });
  expose("doDisconnect", () => doDisconnect, (value) => {
    doDisconnect = value;
  });
  expose("refreshSources", () => refreshSources, (value) => {
    refreshSources = value;
  });
  expose("saveTally", () => saveTally, (value) => {
    saveTally = value;
  });
  expose("saveAudioMode", () => saveAudioMode, (value) => {
    saveAudioMode = value;
  });
  expose("saveDecoder", () => saveDecoder, (value) => {
    saveDecoder = value;
  });
  expose("saveSplash", () => saveSplash, (value) => {
    saveSplash = value;
  });
  expose("previewSplash", () => previewSplash, (value) => {
    previewSplash = value;
  });
  expose("uploadLogo", () => uploadLogo, (value) => {
    uploadLogo = value;
  });
  expose("loadOsd", () => loadOsd2, (value) => {
    loadOsd2 = value;
  });
  expose("saveOsd", () => saveOsd, (value) => {
    saveOsd = value;
  });
  expose("saveNetSettings", () => saveNetSettings, (value) => {
    saveNetSettings = value;
  });

  // src/services.ts
  var SVC_LABELS = { "ndimon-r": "Decoder", "ndimon-finder": "Finder", "ndimon-control": "Web UI and control" };
  var SERVICE_IDS = { "ndimon-r": "decoder", "ndimon-finder": "finder", "ndimon-control": "control" };
  var loadServices2 = async function() {
    try {
      const d = await api("/api/services");
      const el = document.getElementById("svc-list");
      if (!el || !d.services) return;
      el.innerHTML = d.services.map((s) => {
        const label = SVC_LABELS[s.name] || s.name;
        const running = s.active === "active";
        const stateText = running ? s.sub : s.active;
        return `<div class="svc-row">
        <span class="src-dot ${running ? "on" : ""}" style="${running ? "" : "background:var(--danger)"}"></span>
        <span class="svc-name">${esc(label)}</span>
        <span class="svc-state" style="color:${running ? "var(--live)" : "var(--danger)"}">${esc(stateText)}</span>
        <button class="icon-btn" style="width:26px;height:26px;font-size:.75rem" onclick="restartService('${esc(s.name, 1)}')" title="Restart ${esc(label)}"><i class="bi bi-arrow-clockwise"></i></button>
      </div>`;
      }).join("");
    } catch {
    }
  };
  var RESTART_WARNINGS = {
    "ndimon-r": "Every output shows the splash screen while the decoder restarts.",
    "ndimon-finder": "Source discovery pauses for a few seconds. Outputs keep playing.",
    "ndimon-control": "The web UI is unavailable for a few seconds. Reload the page afterwards."
  };
  var restartService = async function(name) {
    const label = SVC_LABELS[name] || name;
    if (!await confirmDialog(`Restart ${label}?`, RESTART_WARNINGS[name] || "The service is unavailable for a few seconds.", { label: "Restart" })) return;
    toast(`Restarting ${label}\u2026`, "warning");
    const service = SERVICE_IDS[name];
    const r = service ? await confirmedCommand({ service_control: { service, action: "restart" } }, { title: `Restart ${label}?`, label: "Restart" }) : { ok: false, error: `Unknown service: ${name}` };
    if (name === "ndimon-control" && r.ok === false && r.error === "Could not reach the device")
      toast("The API connection closed. Restart status is unconfirmed; reload the page.", "warning");
    else done(r, `${label} restarted`);
    setTimeout(loadServices2, 2e3);
  };
  var restartAllServices = async function() {
    if (!await confirmDialog("Restart decoder and finder?", "Every output drops to the splash screen for a few seconds. The web UI stays up.", { label: "Restart" })) return;
    toast("Restarting decoder and finder\u2026", "warning");
    const results = [];
    for (const name of ["ndimon-finder", "ndimon-r"])
      results.push(await confirmedCommand({ service_control: { service: SERVICE_IDS[name], action: "restart" } }, { title: "Restart decoder and finder?", label: "Restart" }));
    const bad = results.find((r) => r && r.ok === false);
    done(bad || {}, "Decoder and finder restarted");
    setTimeout(loadServices2, 3e3);
  };
  var sysReboot = async function() {
    if (!await confirmDialog("Reboot the device?", "Outputs go dark until the decoder is back, usually under a minute. Saved sources reconnect automatically.", { danger: true, label: "Reboot" })) return;
    const r = await confirmedCommand("reboot", { title: "Reboot the device?", label: "Reboot", danger: true });
    if (done(r, "")) toast("Rebooting\u2026", "warning");
  };
  expose("SVC_LABELS", () => SVC_LABELS);
  expose("loadServices", () => loadServices2, (value) => {
    loadServices2 = value;
  });
  expose("restartService", () => restartService, (value) => {
    restartService = value;
  });
  expose("restartAllServices", () => restartAllServices, (value) => {
    restartAllServices = value;
  });
  expose("sysReboot", () => sysReboot, (value) => {
    sysReboot = value;
  });

  // src/scheduled-reboot.ts
  var selectedRebootDays = function() {
    return [...document.querySelectorAll(".rb-day.active")].map((b) => b.dataset.day);
  };
  var setRebootDays = function(days) {
    const set = new Set(days || []);
    document.querySelectorAll(".rb-day").forEach((b) => {
      b.classList.toggle("active", set.has(b.dataset.day));
    });
  };
  var toggleRebootDay = function(btn) {
    btn.classList.toggle("active");
  };
  var toggleRebootSchedule = function() {
    const on = !!document.getElementById("rb-enabled")?.checked;
    const fields = document.getElementById("rb-fields");
    if (fields) {
      fields.style.opacity = on ? "1" : ".5";
      fields.style.pointerEvents = on ? "" : "none";
    }
  };
  var formatRebootNext = function(iso) {
    if (!iso) return "";
    try {
      return new Date(iso).toLocaleString(void 0, { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
    } catch {
      return "";
    }
  };
  var loadRebootNext = async () => {
    const d = await api("/v1/DeviceSettings/reboot-schedule");
    return d.next;
  };
  var showRebootNext = (enabled, next) => {
    const nextEl = document.getElementById("rb-next");
    if (!nextEl) return;
    const when = formatRebootNext(next);
    nextEl.textContent = enabled && when ? `Next reboot: ${when}.` : "If the box is off at that time, it will not reboot when it next starts.";
  };
  var loadRebootSchedule2 = async function() {
    try {
      const state = await nativeState();
      const d = state.device.reboot_schedule;
      const en = document.getElementById("rb-enabled");
      if (en) en.checked = !!d.enabled;
      setVal("rb-time", d.time || "04:00");
      setRebootDays(d.days && d.days.length ? d.days : ["sun", "mon", "tue", "wed", "thu", "fri", "sat"]);
      toggleRebootSchedule();
      acceptNativeRevision("reboot", state.revision);
      showRebootNext(!!d.enabled, await loadRebootNext());
    } catch {
    }
  };
  var saveRebootSchedule = async function() {
    const enabled = !!document.getElementById("rb-enabled")?.checked;
    let time = (document.getElementById("rb-time")?.value || "").trim();
    if (/^\d{2}:\d{2}:\d{2}$/.test(time)) time = time.slice(0, 5);
    const days = selectedRebootDays();
    if (enabled && !days.length) {
      toast("Select at least one day", "warning");
      return;
    }
    const daysOut = days.length ? days : ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
    try {
      const r = await nativeCommand({ set_device: devicePatch({ reboot_schedule: { enabled, time, days: daysOut } }) }, await nativeRevision("reboot"));
      if (!r.ok) {
        toast(resultText(r, "Save failed"), "danger");
        return;
      }
      acceptNativeRevision("reboot", r.revision);
      let next;
      try {
        next = await loadRebootNext();
      } catch {
      }
      const when = formatRebootNext(next);
      toast(enabled ? when ? `Scheduled \u2014 next ${when}` : "Scheduled reboot saved" : "Scheduled reboot off");
      showRebootNext(enabled, next);
    } catch {
      toast("Save failed", "danger");
    }
  };
  expose("selectedRebootDays", () => selectedRebootDays, (value) => {
    selectedRebootDays = value;
  });
  expose("setRebootDays", () => setRebootDays, (value) => {
    setRebootDays = value;
  });
  expose("toggleRebootDay", () => toggleRebootDay, (value) => {
    toggleRebootDay = value;
  });
  expose("toggleRebootSchedule", () => toggleRebootSchedule, (value) => {
    toggleRebootSchedule = value;
  });
  expose("formatRebootNext", () => formatRebootNext, (value) => {
    formatRebootNext = value;
  });
  expose("loadRebootSchedule", () => loadRebootSchedule2, (value) => {
    loadRebootSchedule2 = value;
  });
  expose("saveRebootSchedule", () => saveRebootSchedule, (value) => {
    saveRebootSchedule = value;
  });

  // src/ndi-groups.ts
  var ndiGroups = [];
  var renderNdiGroups = function() {
    const container = document.getElementById("ndi-grp-tags");
    if (!container) return;
    container.innerHTML = ndiGroups.map(
      (g, i) => `<span class="chip chip-blue" style="font-size:.76rem;padding:3px 6px 3px 9px">
      ${esc(g)}<button type="button" class="btn-close btn-close-white" style="font-size:.5rem;margin-left:4px" aria-label="Remove ${esc(g)}" onclick="removeNdiGroup(${i})"></button>
    </span>`
    ).join("");
  };
  var addNdiGroup = function() {
    const input = document.getElementById("ndi-grp-input");
    const val = (input.value || "").trim();
    if (!val || ndiGroups.includes(val)) {
      input.value = "";
      return;
    }
    ndiGroups.push(val);
    input.value = "";
    renderNdiGroups();
  };
  var removeNdiGroup = function(idx) {
    ndiGroups.splice(idx, 1);
    if (ndiGroups.length === 0) ndiGroups.push("public");
    renderNdiGroups();
  };
  var getNdiGroupsString2 = function() {
    return ndiGroups.length ? ndiGroups.join(",") : "public";
  };
  var setNdiGroupsFromString2 = function(s) {
    ndiGroups = (s || "public").split(",").map((g) => g.trim()).filter(Boolean);
    if (ndiGroups.length === 0) ndiGroups.push("public");
    renderNdiGroups();
  };
  document.addEventListener("DOMContentLoaded", () => {
    const inp = document.getElementById("ndi-grp-input");
    if (inp) inp.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        addNdiGroup();
      }
    });
  });
  expose("ndiGroups", () => ndiGroups, (value) => {
    ndiGroups = value;
  });
  expose("renderNdiGroups", () => renderNdiGroups, (value) => {
    renderNdiGroups = value;
  });
  expose("addNdiGroup", () => addNdiGroup, (value) => {
    addNdiGroup = value;
  });
  expose("removeNdiGroup", () => removeNdiGroup, (value) => {
    removeNdiGroup = value;
  });
  expose("getNdiGroupsString", () => getNdiGroupsString2, (value) => {
    getNdiGroupsString2 = value;
  });
  expose("setNdiGroupsFromString", () => setNdiGroupsFromString2, (value) => {
    setNdiGroupsFromString2 = value;
  });

  // src/backup-restore-network.ts
  var NET_PENDING_KEY = "ndimon-net-pending";
  var SECTION_LABELS = {
    outputs: "Outputs and sources",
    presets: "Source presets",
    discovery: "Discovery, groups, extra IPs",
    transport: "Transport",
    display: "Splash and OSD",
    device: "Decoder, watchdog, NTP, reboot schedule",
    identity: "Device name",
    auth: "Admin password",
    network: "Network",
    services: "Decoder restart"
  };
  var restorePlan = null;
  var undoName = "";
  var netTimer = null;
  var netPoll = null;
  var modalOf = function(id) {
    return bootstrap.Modal.getOrCreateInstance(document.getElementById(id));
  };
  var hideModal = function(id) {
    const dialog = document.getElementById(id);
    const modal = bootstrap.Modal.getInstance(dialog);
    if (!modal) return;
    if (dialog.classList.contains("show")) {
      const hideWhenShown = () => modal.hide();
      dialog.addEventListener("shown.bs.modal", hideWhenShown, { once: true });
      dialog.addEventListener("hidden.bs.modal", () => dialog.removeEventListener("shown.bs.modal", hideWhenShown), { once: true });
    }
    modal.hide();
  };
  var confirmDialog2 = async function(title, body, opts = {}) {
    const shown = document.getElementById("confirm-modal");
    if (shown.style.display && shown.style.display !== "none" && !shown.classList.contains("show")) {
      await new Promise((resolve) => shown.addEventListener("hidden.bs.modal", () => resolve(), { once: true }));
    }
    return new Promise((resolve) => {
      const m = document.getElementById("confirm-modal");
      setText("confirm-title", title);
      document.getElementById("confirm-body").textContent = body;
      const ok = document.getElementById("confirm-ok");
      ok.className = "btn btn-sm " + (opts.danger ? "btn-danger" : "btn-primary");
      ok.textContent = opts.label || "Confirm";
      let settled = false;
      const finish = (v) => {
        if (!settled) {
          settled = true;
          resolve(v);
        }
      };
      ok.onclick = () => {
        finish(true);
        hideModal("confirm-modal");
      };
      m.addEventListener("hidden.bs.modal", () => finish(false), { once: true });
      modalOf("confirm-modal").show();
      setTimeout(() => ok.focus(), 200);
    });
  };
  var done2 = function(r, okMsg) {
    if (r && r.ok === false) {
      toast(resultText(r, "Request failed"), "danger");
      return false;
    }
    if (okMsg) toast(okMsg);
    return true;
  };
  var setBusy = function(btn, on, label) {
    if (!btn) return;
    if (on) {
      if (!btn.dataset.label) btn.dataset.label = btn.innerHTML;
      btn.disabled = true;
      btn.innerHTML = `<span class="spinner-border spinner-border-sm me-1"></span>${label || "Working\u2026"}`;
    } else {
      btn.disabled = false;
      if (btn.dataset.label) btn.innerHTML = btn.dataset.label;
    }
  };
  var persistNetPending = function(info) {
    if (!info || !info.token) {
      sessionStorage.removeItem(NET_PENDING_KEY);
      return;
    }
    sessionStorage.setItem(NET_PENDING_KEY, JSON.stringify({
      token: info.token,
      address: info.address || "",
      deadline: info.deadline || 0,
      mode: info.mode || ""
    }));
  };
  var readNetPending = function() {
    try {
      return JSON.parse(sessionStorage.getItem(NET_PENDING_KEY) || "null");
    } catch {
      return null;
    }
  };
  var clearNetPending = function() {
    sessionStorage.removeItem(NET_PENDING_KEY);
    clearInterval(netTimer);
    clearInterval(netPoll);
    netTimer = netPoll = null;
  };
  var restoreModalFooter = function(mode) {
    const foot = document.getElementById("bk-modal-footer");
    if (mode === "result") {
      foot.innerHTML = `<button class="btn btn-secondary btn-sm" onclick="hideModal('bk-modal')">Close</button><button class="btn btn-primary btn-sm" id="bk-result-net" style="display:none" onclick="resumeNetWait()">Confirm new address</button>`;
      return;
    }
    foot.innerHTML = `<button class="btn btn-secondary btn-sm" id="bk-cancel-btn" onclick="hideModal('bk-modal')">Cancel</button><button class="btn btn-primary btn-sm" id="bk-restore-btn" onclick="confirmRestore()">Restore</button>`;
  };
  var syncRestoreButton = function() {
    const btn = document.getElementById("bk-restore-btn");
    if (!btn) return;
    btn.disabled = !document.querySelector("#bk-summary input[type=checkbox]:checked");
  };
  var consumeNetConfirm2 = async function() {
    if (location.hash.startsWith("#netprofile=")) {
      const token2 = location.hash.slice("#netprofile=".length);
      history.replaceState(null, "", location.pathname + location.search);
      await window.NDIMON_NETWORK.resumeToken(token2);
      return "";
    }
    if (!location.hash.startsWith("#netconfirm=")) return "";
    const token = decodeURIComponent(location.hash.slice("#netconfirm=".length));
    history.replaceState(null, "", location.pathname + location.search);
    if (!await confirmDialog2("Keep network changes?", "Keep the new management network settings?", { label: "Keep changes" })) return "Network settings remain unconfirmed and will revert.";
    try {
      const r = await fetch("/api/net-confirm", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ token })
      });
      const j = await r.json().catch(() => ({}));
      if (j.ok || r.status === 410) clearNetPending();
      return j.ok ? "Network settings confirmed." : j.error || "This confirmation has expired. The device has returned to its previous settings.";
    } catch {
      return "Could not confirm the network change.";
    }
  };
  var downloadBackup = async function() {
    const btn = document.getElementById("bk-dl-btn");
    setBusy(btn, true, "Preparing\u2026");
    try {
      const net = document.getElementById("bk-network").checked ? "1" : "0";
      const pw = document.getElementById("bk-password").checked ? "1" : "0";
      const r = await fetch(`/v1/Backup/export?network=${net}&password=${pw}`);
      if (r.status === 401) {
        showLogin();
        return;
      }
      if (!r.ok) {
        toast("Could not build the backup", "danger");
        return;
      }
      const blob = await r.blob();
      const cd = r.headers.get("Content-Disposition") || "";
      const match = cd.match(/filename="([^"]+)"/);
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = match ? match[1] : "ndimon-backup.json";
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 1e3);
    } catch {
      toast("Could not download the backup", "danger");
    } finally {
      setBusy(btn, false);
    }
  };
  var inspectBackup = async function(input) {
    const file = input.files && input.files[0];
    input.value = "";
    if (!file) return;
    const btn = document.getElementById("bk-up-btn");
    setBusy(btn, true, "Reading backup\u2026");
    try {
      let doc;
      try {
        doc = JSON.parse(await file.text());
      } catch {
        toast("Not a valid backup file", "danger");
        return;
      }
      const r = await fetch("/v1/Backup/inspect", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(doc)
      });
      if (r.status === 401) {
        showLogin();
        return;
      }
      const plan = await r.json().catch(() => ({}));
      if (!r.ok || !plan.ok) {
        toast(plan.error || "Backup was rejected", "danger");
        return;
      }
      restorePlan = plan;
      restorePlan.revision = (await nativeState()).revision;
      const src = plan.source_device || {};
      const app = plan.app || {};
      const rows = (plan.sections || []).map((s) => `
      <div class="restore-row">
        <div class="form-check">
          <input class="form-check-input" type="checkbox" value="${esc(s.key)}" id="bk-${esc(s.key)}" ${s.selected ? "checked" : ""} onchange="syncRestoreButton()">
          <label class="form-check-label" for="bk-${esc(s.key)}"><strong>${esc(s.label)}</strong>
            <span class="badge ${s.changed ? "bg-warning text-dark" : "bg-secondary"} ms-1">${s.changed ? "Changed" : "Unchanged"}</span></label>
        </div>
        <div class="form-text" style="white-space:pre-wrap">${esc(s.summary || "")}</div>
      </div>`).join("");
      const net = plan.network || {};
      const netLine = !net.present ? "" : net.needs_prompt ? '<div class="alert alert-warning py-2 mb-3">Network settings differ. You will be asked before they are overwritten.</div>' : net.applicable === false ? `<div class="alert alert-secondary py-2 mb-3">${esc(net.message || "")}</div>` : '<div class="form-text mb-3">Network settings match. They will be left as they are.</div>';
      const extra = (plan.extra_outputs || []).length ? `<div class="form-text">Output ${esc(plan.extra_outputs.join(", "))} is in the file and will be stored. This device may not have that output.</div>` : "";
      const when = plan.created_at ? new Date(plan.created_at).toLocaleString() : "";
      restoreModalFooter("choose");
      document.getElementById("bk-modal-title").textContent = "Restore backup";
      document.getElementById("bk-summary").innerHTML = `<div class="mb-3">From <strong>${esc(src.ndi_alias || src.hostname || "unknown device")}</strong>` + (when ? ` \xB7 ${esc(when)}` : "") + (app.firmware ? ` \xB7 firmware ${esc(app.firmware)}` : "") + `</div>${netLine}${rows}${extra}<div class="form-text mt-3">NDI groups keep the case they were saved with. Leave Device name off when setting up a second receiver from this file. Restoring selected settings restarts the decoder for a few seconds.</div>`;
      modalOf("bk-modal").show();
      syncRestoreButton();
    } catch {
      toast("Could not read the backup", "danger");
    } finally {
      setBusy(btn, false);
    }
  };
  var confirmRestore = function() {
    if (!restorePlan) return;
    restorePlan.chosen = [...document.querySelectorAll("#bk-summary input[type=checkbox]:checked")].map((el) => el.value);
    if (!restorePlan.chosen.length && !(restorePlan.network && restorePlan.network.needs_prompt)) {
      toast("Choose at least one section to restore", "warning");
      return;
    }
    const net = restorePlan.network || {};
    if (net.needs_prompt) {
      const warns = (net.warnings || []).map((w) => `<div class="alert alert-warning py-2">${esc(w)}</div>`).join("");
      const ack = net.duplicate ? `<div class="form-check mt-2"><input class="form-check-input" type="checkbox" id="net-ack" onchange="document.getElementById('net-use').disabled=!this.checked"><label class="form-check-label" for="net-ack">I understand another device is using this address</label></div>` : "";
      document.getElementById("net-modal-body").innerHTML = `<p>${esc(net.message || "")}</p>${warns}${ack}`;
      const use = document.getElementById("net-use");
      use.disabled = !!net.duplicate;
      hideModal("bk-modal");
      modalOf("net-modal").show();
      setTimeout(() => document.getElementById("net-keep")?.focus(), 200);
      return;
    }
    finishRestore(false);
  };
  var resultRows = function(results) {
    return Object.entries(results || {}).map(([key, v]) => {
      const label = SECTION_LABELS[key] || key;
      const vlanFailures = (v && v.vlans || []).filter((vlan) => vlan.ok === false).map((vlan) => `<div class="restore-row"><span class="result-fail">Failed</span> \xB7 VLAN ${esc(vlan.id)} \u2014 ${esc(vlan.error || "error")}</div>`).join("");
      if (v && v.ok === false)
        return `<div class="restore-row"><span class="result-fail">Failed</span> \xB7 ${esc(label)} \u2014 ${esc(v.error || "error")}</div>` + vlanFailures;
      if (v && v.skipped)
        return `<div class="restore-row"><span class="result-skip">Skipped</span> \xB7 ${esc(label)} \u2014 ${esc(v.skipped)}</div>` + vlanFailures;
      const extra = v && v.warning ? ` \u2014 ${esc(v.warning)}` : v && v.pending ? " \u2014 waiting for address confirmation" : "";
      return `<div class="restore-row"><span class="result-ok">Restored</span> \xB7 ${esc(label)}${extra}</div>` + vlanFailures;
    }).join("");
  };
  var showRestoreResult = function(out, title) {
    restoreModalFooter("result");
    document.getElementById("bk-modal-title").textContent = title || "Restore complete";
    const failed = Object.entries(out.results || {}).some(([, v]) => v && v.ok === false);
    document.getElementById("bk-summary").innerHTML = (failed ? '<div class="alert alert-warning py-2">Restored, with a problem in one or more sections.</div>' : '<div class="alert alert-success py-2">Settings restored.</div>') + resultRows(out.results) + (out.noop ? '<div class="form-text">Nothing was selected, so the decoder was left running.</div>' : "");
    const netBtn = document.getElementById("bk-result-net");
    if (netBtn && out.network && out.network.token) netBtn.style.display = "";
    modalOf("bk-modal").show();
  };
  var refreshAfterRestore = function() {
    loadSnapshots2();
    loadSysInfo();
    try {
      loadSettings();
    } catch {
    }
  };
  var restoreCall = async (url, body, owner) => {
    let token;
    for (let attempt = 0; ; attempt++) {
      const r = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ ...body, ...token ? { expect_impact: token } : {} })
      });
      if (r.status === 401) {
        showLogin();
        return null;
      }
      const out = await r.json().catch(() => ({}));
      const impact = out.impact;
      if (out.error !== "confirmation_required" || !impact?.token || attempt === 2) return { ok: r.ok, out };
      if (attempt > 0) {
        hideModal(owner);
        if (!await nativeConfirm("Apply change", impact.lines?.join(" ") || "This change briefly interrupts the device.", { label: "Apply" })) {
          return { ok: false, out, cancelled: true };
        }
        modalOf(owner).show();
      }
      token = impact.token;
    }
  };
  var finishRestore = async function(applyNetwork) {
    if (!restorePlan) return;
    if (!(restorePlan.chosen || []).length && !applyNetwork) {
      hideModal("net-modal");
      toast("Nothing to restore", "warning");
      return;
    }
    hideModal("net-modal");
    const btn = document.getElementById("bk-restore-btn");
    setBusy(btn, true, "Restoring\u2026");
    document.getElementById("bk-modal-title").textContent = "Restoring\u2026";
    document.getElementById("bk-summary").innerHTML = "<p>Writing settings. The picture goes to the splash screen for a few seconds if the decoder needs to restart.</p>";
    modalOf("bk-modal").show();
    try {
      const reply = await restoreCall("/v1/Backup/apply", {
        import_id: restorePlan.import_id,
        sections: restorePlan.chosen || [],
        apply_network: !!applyNetwork,
        expect_revision: restorePlan.revision
      }, "bk-modal");
      if (!reply) return;
      if (reply.cancelled) {
        hideModal("bk-modal");
        return;
      }
      const out = reply.out;
      if (!reply.ok || !out.ok) {
        toast(out.error || "Restore failed", "danger");
        hideModal("bk-modal");
        return;
      }
      showRestoreResult(out);
      if (out.network && out.network.token) persistNetPending(out.network);
      refreshAfterRestore();
      if (out.relogin) showLogin();
    } catch {
      toast("Restore failed", "danger");
      hideModal("bk-modal");
    } finally {
      setBusy(btn, false);
    }
  };
  var netConfirmHref = function(info) {
    if (!info || !info.token) return "";
    const port = location.port ? ":" + location.port : "";
    if (info.address)
      return `${location.protocol}//${info.address}${port}/#netconfirm=${info.token}`;
    return `${location.origin}/#netconfirm=${info.token}`;
  };
  var renderNetWait = function(info) {
    const text = document.getElementById("net-wait-text");
    const link = document.getElementById("net-wait-link");
    const href = netConfirmHref(info);
    if (info.address) {
      text.textContent = "Open this address from a computer that can reach it. The change reverts on its own if you do not.";
      link.href = href;
      link.textContent = href;
    } else {
      text.textContent = "The new address will appear on the connected display. When it does, this link updates. You can also copy it and replace the host with the address on the screen.";
      link.removeAttribute("href");
      link.textContent = href || `/#netconfirm=${info.token || ""}`;
    }
  };
  var showNetWait = function(info) {
    if (info?.profiles) return window.NDIMON_NETWORK.watch(info);
    persistNetPending(info);
    renderNetWait(info);
    const deadline = (info.deadline || 0) * 1e3;
    clearInterval(netTimer);
    clearInterval(netPoll);
    const tick = () => {
      const left = Math.max(0, Math.round((deadline - Date.now()) / 1e3));
      const el = document.getElementById("net-wait-count");
      if (el) el.textContent = left ? `Reverts in ${left}s if not confirmed.` : "The change is reverting.";
      if (!left) {
        clearInterval(netTimer);
        clearInterval(netPoll);
      }
    };
    tick();
    netTimer = setInterval(tick, 1e3);
    netPoll = setInterval(async () => {
      try {
        await store.refreshObserved();
        const pending = store.observed?.network_pending ?? null;
        if (!pending) {
          clearNetPending();
          hideModal("net-wait");
          toast("The pending network change has ended. Check the address on this page.", "warning", 8e3);
          loadNetwork();
          return;
        }
        const live = pending.management_address || "";
        if (live && live !== info.address) {
          info.address = live;
          persistNetPending(info);
          renderNetWait(info);
        }
      } catch {
      }
    }, 2e3);
    hideModal("bk-modal");
    modalOf("net-wait").show();
  };
  var resumeNetWait2 = async function(quiet) {
    if (window.NDIMON_NETWORK?.resumeStored()) return;
    let info = readNetPending();
    try {
      const pending = await nativePending();
      if (!pending) {
        clearNetPending();
        if (!quiet) toast("No network change is waiting", "warning");
        loadNetwork();
        return;
      }
      info = {
        token: info && info.token || "",
        address: pending.management_address || info && info.address || "",
        deadline: Math.round(Date.now() / 1e3) + pending.remaining_seconds || info && info.deadline || 0,
        mode: info && info.mode || ""
      };
    } catch {
      if (!info || !info.token) {
        if (!quiet) toast("Could not load the pending network change", "danger");
        return;
      }
    }
    if (!info.token) {
      if (!quiet) toast("The confirmation link is no longer available. Cancel from the Network card if you need to revert.", "warning", 8e3);
      return;
    }
    showNetWait(info);
  };
  var copyNetWaitLink = async function() {
    const link = document.getElementById("net-wait-link");
    const text = link && (link.href && link.href !== location.href + "#" ? link.href : link.textContent) || "";
    if (!text) return;
    if (navigator.clipboard && navigator.clipboard.writeText) {
      try {
        await navigator.clipboard.writeText(text);
        toast("Link copied");
        return;
      } catch {
      }
    }
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try {
      if (!document.execCommand("copy")) throw new Error("copy denied");
      toast("Link copied");
    } catch {
      toast("Could not copy the link", "danger");
    } finally {
      ta.remove();
    }
  };
  var cancelNetworkChange = async function() {
    const r = await nativeCommand("rollback_network");
    if (!done2(r, "")) return;
    clearInterval(netTimer);
    clearInterval(netPoll);
    clearNetPending();
    hideModal("net-wait");
    toast("Network change reverted", "warning");
    loadNetwork();
  };
  var loadSnapshots2 = async function() {
    const box = document.getElementById("bk-snaps");
    if (!box) return;
    try {
      const d = await api("/v1/Backup/snapshots");
      const snaps = d.snapshots || [];
      if (!snaps.length) {
        box.innerHTML = "";
        return;
      }
      const newest = snaps[0];
      const when = newest.mtime ? new Date(newest.mtime).toLocaleString() : "";
      box.innerHTML = `<button class="btn btn-outline-warning btn-sm" onclick="askUndo('${esc(newest.name, true)}')">Undo last restore</button><div class="form-text mt-1">${esc(when)}</div>`;
    } catch {
    }
  };
  var askUndo = function(name) {
    undoName = name;
    modalOf("bk-undo-modal").show();
  };
  var commitUndo = async function() {
    const btn = document.getElementById("bk-undo-go");
    setBusy(btn, true, "Restoring\u2026");
    try {
      const r = await confirmedCommand({ undo_restore: { snapshot: undoName } }, { title: "Undo last restore", label: "Undo restore" });
      hideModal("bk-undo-modal");
      if (r.cancelled) return;
      if (!r.ok) {
        toast(resultText(r, "Undo failed"), "danger");
        return;
      }
      const out = r.result ?? {};
      showRestoreResult(out, "Previous settings restored");
      if (out.network && out.network.token) persistNetPending(out.network);
      refreshAfterRestore();
      if (out.relogin) showLogin();
    } catch {
      toast("Undo failed", "danger");
    } finally {
      setBusy(btn, false);
    }
  };
  expose("NET_PENDING_KEY", () => NET_PENDING_KEY);
  expose("SECTION_LABELS", () => SECTION_LABELS);
  expose("restorePlan", () => restorePlan, (value) => {
    restorePlan = value;
  });
  expose("undoName", () => undoName, (value) => {
    undoName = value;
  });
  expose("netTimer", () => netTimer, (value) => {
    netTimer = value;
  });
  expose("netPoll", () => netPoll, (value) => {
    netPoll = value;
  });
  expose("modalOf", () => modalOf, (value) => {
    modalOf = value;
  });
  expose("hideModal", () => hideModal, (value) => {
    hideModal = value;
  });
  expose("confirmDialog", () => confirmDialog2, (value) => {
    confirmDialog2 = value;
  });
  expose("done", () => done2, (value) => {
    done2 = value;
  });
  expose("setBusy", () => setBusy, (value) => {
    setBusy = value;
  });
  expose("persistNetPending", () => persistNetPending, (value) => {
    persistNetPending = value;
  });
  expose("readNetPending", () => readNetPending, (value) => {
    readNetPending = value;
  });
  expose("clearNetPending", () => clearNetPending, (value) => {
    clearNetPending = value;
  });
  expose("restoreModalFooter", () => restoreModalFooter, (value) => {
    restoreModalFooter = value;
  });
  expose("syncRestoreButton", () => syncRestoreButton, (value) => {
    syncRestoreButton = value;
  });
  expose("consumeNetConfirm", () => consumeNetConfirm2, (value) => {
    consumeNetConfirm2 = value;
  });
  expose("downloadBackup", () => downloadBackup, (value) => {
    downloadBackup = value;
  });
  expose("inspectBackup", () => inspectBackup, (value) => {
    inspectBackup = value;
  });
  expose("confirmRestore", () => confirmRestore, (value) => {
    confirmRestore = value;
  });
  expose("resultRows", () => resultRows, (value) => {
    resultRows = value;
  });
  expose("showRestoreResult", () => showRestoreResult, (value) => {
    showRestoreResult = value;
  });
  expose("refreshAfterRestore", () => refreshAfterRestore, (value) => {
    refreshAfterRestore = value;
  });
  expose("finishRestore", () => finishRestore, (value) => {
    finishRestore = value;
  });
  expose("netConfirmHref", () => netConfirmHref, (value) => {
    netConfirmHref = value;
  });
  expose("renderNetWait", () => renderNetWait, (value) => {
    renderNetWait = value;
  });
  expose("showNetWait", () => showNetWait, (value) => {
    showNetWait = value;
  });
  expose("resumeNetWait", () => resumeNetWait2, (value) => {
    resumeNetWait2 = value;
  });
  expose("copyNetWaitLink", () => copyNetWaitLink, (value) => {
    copyNetWaitLink = value;
  });
  expose("cancelNetworkChange", () => cancelNetworkChange, (value) => {
    cancelNetworkChange = value;
  });
  expose("loadSnapshots", () => loadSnapshots2, (value) => {
    loadSnapshots2 = value;
  });
  expose("askUndo", () => askUndo, (value) => {
    askUndo = value;
  });
  expose("commitUndo", () => commitUndo, (value) => {
    commitUndo = value;
  });

  // src/helpers.ts
  var handle4012 = function(r) {
    if (r.status === 401) {
      showLogin();
      throw new Error("unauthorized");
    }
    return r;
  };
  var api2 = async function(url) {
    return fetch(url).then(handle4012).then((r) => r.json());
  };
  var del = async function(url) {
    return post2(url, void 0, "DELETE");
  };
  var post2 = async function(url, body, method = "POST", opts = {}) {
    let j = await postOnce(url, body, method);
    for (let round = 0; opts.confirm !== false && round < 2 && j && j.error === "confirmation_required" && j.impact?.token; round++) {
      const text = (j.impact.lines || []).join(" ") || "This change briefly interrupts the device.";
      if (!await confirmDialog("Apply change", text, { label: "Apply" })) return { ok: false, error: "Not applied", cancelled: true };
      j = await postOnce(url, { ...body || {}, expect_impact: j.impact.token }, method);
    }
    return j;
  };
  var postOnce = async function(url, body, method = "POST") {
    try {
      const r = await fetch(url, { method, headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }).then(handle4012);
      const j = await r.json().catch(() => ({}));
      if (!r.ok && j.ok !== false) return { ok: false, error: j.error || `HTTP ${r.status}` };
      return j;
    } catch (e) {
      if (e && e.message === "unauthorized") return { ok: false, error: "Signed out" };
      return { ok: false, error: "Could not reach the device" };
    }
  };
  var setText2 = function(id, v) {
    const el = document.getElementById(id);
    if (el) el.textContent = v;
  };
  var setVal2 = function(id, v) {
    const el = document.getElementById(id);
    if (el && v != null) el.value = v;
  };
  var setCheck2 = function(id, v) {
    const el = document.getElementById(id);
    if (el) el.checked = v;
  };
  var setBar2 = function(id, pct, isGreen) {
    const el = document.getElementById(id);
    if (!el) return;
    el.style.width = Math.min(100, Math.max(0, pct)) + "%";
    el.className = "bar-fill " + (pct > 85 ? "bar-danger" : pct > 65 ? "bar-warn" : isGreen ? "bar-green" : "bar-blue");
  };
  var fmtCodec2 = function(c) {
    if (!c || c === "none") return "\u2014";
    if (c === "uncompressed") return "NDI (SpeedHQ)";
    return String(c).toUpperCase().replace("H264", "H.264").replace("H265", "H.265").replace("HEVC", "H.265");
  };
  var esc2 = function(s, forAttr) {
    const html = String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
    if (forAttr) return html.replace(/\\/g, "\\\\").replace(/'/g, "\\'");
    return html;
  };
  expose("handle401", () => handle4012, (value) => {
    handle4012 = value;
  });
  expose("api", () => api2, (value) => {
    api2 = value;
  });
  expose("del", () => del, (value) => {
    del = value;
  });
  expose("post", () => post2, (value) => {
    post2 = value;
  });
  expose("postOnce", () => postOnce, (value) => {
    postOnce = value;
  });
  expose("setText", () => setText2, (value) => {
    setText2 = value;
  });
  expose("setVal", () => setVal2, (value) => {
    setVal2 = value;
  });
  expose("setCheck", () => setCheck2, (value) => {
    setCheck2 = value;
  });
  expose("setBar", () => setBar2, (value) => {
    setBar2 = value;
  });
  expose("fmtCodec", () => fmtCodec2, (value) => {
    fmtCodec2 = value;
  });
  expose("esc", () => esc2, (value) => {
    esc2 = value;
  });

  // src/plugins.ts
  var pluginStatusListeners = /* @__PURE__ */ new Set();
  var pluginFragments = /* @__PURE__ */ new Map();
  var pluginListing = { plugins: [], feed: { plugins: [] } };
  var pluginUIBusy = false;
  var initializers = () => window;
  var apiGet = function(url) {
    return api(url);
  };
  var reconcilePluginFragments = function(plugins) {
    const enabled = plugins.filter((plugin) => plugin.state === "enabled");
    for (const [name, fragment] of pluginFragments) {
      if (enabled.some((plugin) => plugin.name === name && plugin.version === fragment.version)) continue;
      pluginFragments.delete(name);
      delete initializers()["NDIMON_PLUGIN_INIT_" + name];
      try {
        fragment.cleanup?.();
      } catch (e) {
        console.warn("[Plugin] cleanup", e);
      }
      fragment.cleanup = void 0;
      fragment.pane.remove();
      fragment.tab.remove();
    }
  };
  var renderPluginStateError = function() {
    const banner = document.getElementById("plugin-state-warning");
    if (banner) banner.style.display = pluginListing.plugins_state_error ? "" : "none";
    const button = document.getElementById("plugin-state-reset");
    if (button) button.disabled = pluginUIBusy;
  };
  var settledPluginJob = () => {
    const observed = store.observed?.domains.plugins;
    return observed?.status === "applying" ? void 0 : observed ?? { status: "in_sync", applied_revision: null, detail: null };
  };
  var waitForPluginJob = async function(timeoutMs = 31e4) {
    try {
      await store.refreshObserved();
    } catch {
    }
    const now = settledPluginJob();
    if (now) return now;
    return new Promise((resolve) => {
      const finish = (value) => {
        unsubscribe();
        clearTimeout(timer);
        resolve(value);
      };
      const unsubscribe = store.subscribe(() => {
        const settled = settledPluginJob();
        if (settled) finish(settled);
      });
      const timer = setTimeout(() => finish(void 0), timeoutMs);
    });
  };
  var resetPluginState = async function() {
    if (pluginUIBusy) return;
    if (!await confirmDialog("Reset plugin state", "Clear saved plugin enable flags? Installed plugins and their settings are kept. All plugins will be disabled.", { danger: true, label: "Reset state" })) return;
    pluginUIBusy = true;
    renderPluginStateError();
    try {
      const result = await confirmedCommand("reset_plugin_state", { title: "Reset plugin state", label: "Reset state", danger: true });
      if (!result.ok) throw new Error(resultText(result, "Could not reset plugin state"));
      pluginListing.plugins_state_error = null;
      toast("Plugin state reset", "success");
    } catch (e) {
      toast(e.message, "danger");
    } finally {
      pluginUIBusy = false;
      renderPluginStateError();
      await loadPlugins2();
    }
  };
  var notifyPluginStatus2 = function(status) {
    if (Object.prototype.hasOwnProperty.call(status, "plugins_state_error")) {
      pluginListing.plugins_state_error = status.plugins_state_error;
      renderPluginStateError();
    }
    if (Array.isArray(status.plugins)) {
      const previous = new Map(pluginListing.plugins.map((plugin) => [plugin.name, plugin]));
      pluginListing.plugins = status.plugins.map((plugin) => ({ ...previous.get(plugin.name), ...plugin }));
      reconcilePluginFragments(pluginListing.plugins);
      renderPluginList();
    }
    for (const callback of pluginStatusListeners) {
      try {
        callback(status);
      } catch (e) {
        console.warn("[Plugin] status", e);
      }
    }
  };
  if (typeof window !== "undefined") window.NDIMON = {
    get: (url) => apiGet(url),
    post,
    toast,
    onStatus(callback) {
      pluginStatusListeners.add(callback);
      return () => pluginStatusListeners.delete(callback);
    },
    get state() {
      return { ...store.status.decoder, ...liveView2() };
    }
  };
  var loadPlugins2 = async function() {
    try {
      pluginListing = await apiGet("/v1/plugins");
      if (!pluginListing.plugins) throw new Error(pluginListing.error || "Could not load plugins");
      renderPluginList();
      const feed = await apiGet("/v1/plugins/feed");
      pluginListing.feed = feed;
      renderPluginList();
    } catch (e) {
      toast(e.message, "danger");
    }
  };
  var renderPluginList = function() {
    renderPluginStateError();
    const container = document.getElementById("plugin-list");
    if (!container) return;
    container.replaceChildren();
    const feed = pluginListing.feed || {};
    setText("plugin-feed-status", feed.available ? "Feed available" : feed.error || "No feed configured; upload an archive to install.");
    const entries = new Map((pluginListing.plugins || []).map((plugin) => [plugin.name, plugin]));
    for (const entry of feed.plugins || []) if (!entries.has(entry.name)) entries.set(entry.name, { ...entry, title: entry.name, state: "available" });
    if (!entries.size) container.textContent = "No plugins installed.";
    for (const plugin of entries.values()) {
      const row = document.createElement("div");
      row.className = "d-flex align-items-center gap-2 flex-wrap border-bottom py-2";
      row.dataset.plugin = plugin.name;
      const label = document.createElement("span");
      label.className = "me-auto";
      label.textContent = `${plugin.title || plugin.name} ${plugin.version || ""} \xB7 ${plugin.state}${plugin.error ? " \xB7 " + plugin.error : plugin.migration_warning ? " \xB7 " + plugin.migration_warning : ""}`;
      row.appendChild(label);
      const actions = plugin.state === "available" ? [["install", "Install"]] : plugin.state === "broken" ? [["uninstall", "Retry remove"], ["force", "Force remove"]] : plugin.state === "error" ? [["uninstall", "Remove"], ["force", "Force remove"]] : [[plugin.state === "enabled" ? "disable" : "enable", plugin.state === "enabled" ? "Disable" : "Enable"], ["update", "Update"], ["uninstall", "Remove"]];
      for (const [action, text] of actions) {
        const button = document.createElement("button");
        button.className = "btn btn-sm btn-outline-secondary";
        button.textContent = text;
        button.disabled = pluginUIBusy;
        button.onclick = () => runPluginAction(plugin.name, action);
        row.appendChild(button);
      }
      container.appendChild(row);
    }
    const enabled = pluginListing.plugins.filter((p) => p.state === "enabled");
    reconcilePluginFragments(pluginListing.plugins);
    for (const plugin of enabled) {
      if (pluginFragments.has(plugin.name)) continue;
      const pane = document.createElement("div");
      pane.id = "plugin-pane-" + plugin.name;
      pane.className = "card card-body";
      pane.setAttribute("role", "tabpanel");
      const tab = document.createElement("button");
      tab.className = "nav-link";
      tab.type = "button";
      tab.textContent = plugin.title ?? "";
      tab.setAttribute("role", "tab");
      tab.setAttribute("aria-controls", pane.id);
      tab.onclick = () => selectPluginTab(plugin.name);
      document.getElementById("plugin-tabs").appendChild(tab);
      document.getElementById("plugin-panes").appendChild(pane);
      pluginFragments.set(plugin.name, { pane, tab, version: plugin.version });
      mountPluginFragment(plugin.name).catch((e) => {
        pane.textContent = e.message;
      });
    }
    const selected = [...pluginFragments].find(([, f]) => f.tab.classList.contains("active"))?.[0];
    if (enabled.length) selectPluginTab(selected || enabled[0].name);
  };
  var selectPluginTab = function(name) {
    for (const [key, { pane, tab }] of pluginFragments) {
      pane.hidden = key !== name;
      tab.classList.toggle("active", key === name);
      tab.setAttribute("aria-selected", String(key === name));
    }
  };
  var mountPluginFragment = async function(name) {
    const fragment = pluginFragments.get(name);
    if (!fragment) return;
    const current = () => pluginFragments.get(name) === fragment;
    const response = await fetch(`/plugins/${encodeURIComponent(name)}/ui.html`).then(handle401);
    if (!current()) return;
    if (!response.ok) throw new Error("Could not load plugin page");
    const html = await response.text();
    if (!current()) return;
    fragment.pane.innerHTML = html;
    for (const script of [...fragment.pane.querySelectorAll("script")]) {
      const executable = document.createElement("script");
      for (const attr of script.attributes) executable.setAttribute(attr.name, attr.value);
      executable.textContent = script.textContent;
      const loaded = script.src ? new Promise((resolve, reject) => {
        executable.onload = resolve;
        executable.onerror = reject;
      }) : null;
      script.replaceWith(executable);
      if (loaded) {
        await loaded;
        if (!current()) return;
      }
    }
    const init = initializers()["NDIMON_PLUGIN_INIT_" + name];
    if (typeof init === "function") {
      const cleanup = await init(fragment.pane);
      if (!current()) {
        if (typeof cleanup === "function") cleanup();
        return;
      }
      if (typeof cleanup === "function") fragment.cleanup = cleanup;
    }
  };
  var refreshPluginFeed = async function() {
    const result = await post("/v1/plugins/feed/refresh", {});
    if (result.ok === false) return toast(result.error, "danger");
    await loadPlugins2();
  };
  var runPluginAction = async function(name, action) {
    if (pluginUIBusy) return;
    if (["uninstall", "force"].includes(action) && !await confirmDialog("Remove plugin", `Remove ${name}? Its configuration is kept.`, { danger: true, label: "Remove" })) return;
    pluginUIBusy = true;
    try {
      let result;
      if (action === "install") {
        result = await post("/v1/plugins/install", { name, source: "feed", enable: true });
      } else if (action === "update") {
        result = await post(`/v1/plugins/${encodeURIComponent(name)}/update`, {});
      } else {
        const version = pluginListing.plugins.find((plugin) => plugin.name === name)?.version ?? null;
        const command = action === "enable" || action === "disable" ? { set_plugin_enabled: { name, enabled: action === "enable", version } } : { remove_plugin: { name, purge: false, force: action === "force" } };
        result = action === "uninstall" || action === "force" ? await confirmedCommand(command, { title: "Remove plugin", label: "Remove", danger: true }) : await nativeCommand(command);
        if (result.ok) {
          const outcome = await waitForPluginJob();
          if (outcome?.status === "failed") result = { ok: false, error: outcome.detail || "Plugin operation failed" };
          else {
            try {
              pluginListing = { ...pluginListing, ...await apiGet("/v1/plugins") };
            } catch {
            }
            const listed = pluginListing.plugins.find((plugin) => plugin.name === name)?.state;
            result = { ...result, state: listed ?? ("remove_plugin" in command ? "removed" : action === "enable" ? "enabled" : "installed") };
          }
        }
      }
      if (!result.ok) throw new Error(resultText(result, result.log || "Plugin operation failed"));
      toast(`${name}: ${result.state}`, "success");
    } catch (e) {
      toast(e.message, "danger");
    } finally {
      pluginUIBusy = false;
      await loadPlugins2();
    }
  };
  var uploadPlugin = async function() {
    const name = document.getElementById("plugin-upload-name").value.trim();
    const file = document.getElementById("plugin-upload").files[0];
    if (!/^[a-z][a-z0-9-]{1,31}$/.test(name) || !file) return toast("Choose a plugin name and archive", "warning");
    if (pluginUIBusy) return;
    if (!await confirmDialog("Install plugin", `Install ${name}? Only install archives from publishers you trust.`, { label: "Install" })) return;
    pluginUIBusy = true;
    try {
      const response = await fetch("/v1/plugins/upload?name=" + encodeURIComponent(name), { method: "POST", headers: { "Content-Type": "application/octet-stream" }, body: file }).then(handle401);
      const uploaded = await response.json();
      if (!response.ok || !uploaded.ok) throw new Error(uploaded.error || "Upload failed");
      const result = await post("/v1/plugins/install", { name, source: "upload", enable: true });
      if (!result.ok) throw new Error(result.error || "Install failed");
      toast(`${name}: ${result.state}`, "success");
    } catch (e) {
      toast(e.message, "danger");
    } finally {
      pluginUIBusy = false;
      await loadPlugins2();
    }
  };
  expose("pluginStatusListeners", () => pluginStatusListeners);
  expose("pluginFragments", () => pluginFragments);
  expose("pluginListing", () => pluginListing, (value) => {
    pluginListing = value;
  });
  expose("pluginUIBusy", () => pluginUIBusy, (value) => {
    pluginUIBusy = value;
  });
  expose("apiGet", () => apiGet, (value) => {
    apiGet = value;
  });
  expose("reconcilePluginFragments", () => reconcilePluginFragments, (value) => {
    reconcilePluginFragments = value;
  });
  expose("renderPluginStateError", () => renderPluginStateError, (value) => {
    renderPluginStateError = value;
  });
  expose("waitForPluginJob", () => waitForPluginJob, (value) => {
    waitForPluginJob = value;
  });
  expose("resetPluginState", () => resetPluginState, (value) => {
    resetPluginState = value;
  });
  expose("notifyPluginStatus", () => notifyPluginStatus2, (value) => {
    notifyPluginStatus2 = value;
  });
  expose("loadPlugins", () => loadPlugins2, (value) => {
    loadPlugins2 = value;
  });
  expose("renderPluginList", () => renderPluginList, (value) => {
    renderPluginList = value;
  });
  expose("selectPluginTab", () => selectPluginTab, (value) => {
    selectPluginTab = value;
  });
  expose("mountPluginFragment", () => mountPluginFragment, (value) => {
    mountPluginFragment = value;
  });
  expose("refreshPluginFeed", () => refreshPluginFeed, (value) => {
    refreshPluginFeed = value;
  });
  expose("runPluginAction", () => runPluginAction, (value) => {
    runPluginAction = value;
  });
  expose("uploadPlugin", () => uploadPlugin, (value) => {
    uploadPlugin = value;
  });
})();
