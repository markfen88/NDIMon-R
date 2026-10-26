class ApiFailure extends Error {
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
}
class ApiClient {
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
}
const apiClient = new ApiClient();
export {
  ApiClient,
  ApiFailure,
  apiClient
};
