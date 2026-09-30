/** Structured HTTP errors surfaced by the gateway's uniform error handler. */
export class HttpError extends Error {
  constructor(
    readonly statusCode: number,
    readonly code: string,
    message: string,
    /**
     * Gateway-internal origin (the RLN or wallet failure this was mapped
     * from). Logged by the central error handler, never sent to a client —
     * the response body carries only `code` and `message` (invariant I4).
     */
    options?: { cause?: unknown },
  ) {
    super(message, options);
    this.name = 'HttpError';
  }
}
