import { BrowserEventStream } from './BrowserEventStream.ts';
import type { ClientDependencies } from './client-types.ts';

export function browserDependencies(): ClientDependencies {
  return {
    fetch: (input, init) => fetch(input, init),
    openEvents: (url) => new BrowserEventStream(url),
    scheduleReconnect: (callback, delayMs) => window.setTimeout(callback, delayMs),
  };
}
