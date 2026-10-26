import type { ApiError } from './generated/ApiError';
import type { CommandReply } from './generated/CommandReply';
import type { CommandRequest } from './generated/CommandRequest';
import type { DesiredState } from './generated/DesiredState';
import type { Domain } from './generated/Domain';
import type { ErrorEnvelope } from './generated/ErrorEnvelope';
import type { Observed } from './generated/Observed';
import type { Impact } from './generated/Impact';

export class ApiFailure extends Error {
  constructor(readonly status: number, readonly envelope: ErrorEnvelope) {
    super(envelope.message);
    this.name = 'ApiFailure';
  }

  get kind(): string {
    const error = this.envelope.error;
    return typeof error === 'string' ? error : Object.keys(error)[0];
  }
}

export type CommandOptions = Pick<CommandRequest, 'command'> &
  Partial<Pick<CommandRequest, 'expect_revision' | 'expect_impact'>>;
export type ConfirmImpact = (impact: Impact) => Promise<boolean>;
/** `GET /api/v1/session`: whether this browser is signed in and, if so, what a reloaded page needs. */
export interface SessionStatus { logged_in: boolean; csrf?: string; default_password?: boolean }

/** Browser-only v1 client. Confirmation presentation belongs to its caller. */
export class ApiClient {
  private csrf: string | null = null;

  constructor(private readonly fetcher: typeof fetch = (input, init) => globalThis.fetch(input, init)) {}

  setCsrf(csrf: string | null): void {
    this.csrf = csrf;
  }

  get csrfToken(): string | null {
    return this.csrf;
  }

  async login(password: string): Promise<void> {
    const session = await this.request<{ csrf: string }>('/api/v1/session', 'POST', { password });
    this.csrf = session.csrf;
  }

  async logout(): Promise<void> {
    await this.request<void>('/api/v1/session', 'DELETE');
    this.csrf = null;
  }

  /** Session status; anyone may ask, only a signed-in session is given its token. */
  async session(): Promise<SessionStatus> {
    const session = await this.request<SessionStatus>('/api/v1/session', 'GET');
    if (session.csrf) this.csrf = session.csrf;
    return session;
  }

  /** The server replaces every session, so the reply carries this one's new token. */
  async changePassword(current_password: string, new_password: string): Promise<void> {
    const session = await this.request<{ csrf: string }>('/api/v1/session/password', 'POST', { current_password, new_password });
    this.csrf = session.csrf;
  }

  state(): Promise<DesiredState> {
    return this.get('/api/v1/state');
  }

  domain<D extends Domain>(domain: D): Promise<{ revision: number; domain_revision: number | null; state: DesiredState[D] }> {
    return this.get(`/api/v1/state/${domain}`);
  }

  observed(): Promise<Observed> {
    return this.get('/api/v1/observed');
  }

  get<T>(path: string): Promise<T> {
    return this.request<T>(path, 'GET');
  }

  async command(request: CommandOptions, confirm?: ConfirmImpact): Promise<CommandReply> {
    // Retain the editor's revision and original arguments across every retry.
    let body = { ...request, confirm: false };
    for (let round = 0; ; round++) {
      try {
        return await this.request<CommandReply>('/api/v1/commands', 'POST', body);
      } catch (error) {
        if (!(error instanceof ApiFailure) || error.status !== 409 || !confirm || round >= 2) throw error;
        const refusal = error.envelope.error;
        if (typeof refusal === 'string' || !('confirmation_required' in refusal)) throw error;
        const impact = refusal.confirmation_required.impact;
        if (!await confirm(impact)) throw error;
        body = { ...body, expect_impact: impact.token };
      }
    }
  }

  private async request<T>(path: string, method: string, body?: unknown): Promise<T> {
    const headers = new Headers({ Accept: 'application/json' });
    if (body !== undefined) headers.set('Content-Type', 'application/json');
    if (method !== 'GET' && this.csrf) headers.set('X-CSRF-Token', this.csrf);
    const response = await this.fetcher(path, {
      method, headers, credentials: 'same-origin', cache: 'no-store',
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    if (!response.ok) {
      // Branch on the generated enum, never the human-readable message.
      const envelope: ErrorEnvelope = await response.json();
      throw new ApiFailure(response.status, envelope);
    }
    if (response.status === 204) return undefined as T;
    return response.json() as Promise<T>;
  }
}

export type { ApiError };

// Defer fetch lookup so the session CSRF wrapper remains authoritative.
export const apiClient = new ApiClient();
