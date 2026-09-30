/**
 * Production entrypoint: load config from the environment, build the server
 * with background workers, listen, and shut down cleanly on SIGINT/SIGTERM.
 * Run with `node dist/main.js` (or `pnpm --filter @utexo/minimal-gateway start`).
 */
import { loadConfig } from './config.js';
import { buildServer } from './server.js';

const config = loadConfig();
const app = await buildServer({ config, startWorkers: true });
await app.listen({ host: config.host, port: config.port });

let shuttingDown = false;
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.on(signal, () => {
    if (shuttingDown) return;
    shuttingDown = true;
    app.log.info({ signal }, 'shutting down');
    void app
      .close()
      .then(() => process.exit(0))
      .catch((error: unknown) => {
        app.log.error({ err: error }, 'shutdown failed');
        process.exit(1);
      });
  });
}
