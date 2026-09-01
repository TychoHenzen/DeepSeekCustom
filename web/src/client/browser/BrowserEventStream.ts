import type { EventMessage, EventStream } from './client-types.ts';

export class BrowserEventStream implements EventStream {
  readonly #source: EventSource;

  constructor(url: string) {
    this.#source = new EventSource(url);
  }

  set onerror(listener: (() => void) | null) {
    this.#source.onerror = listener;
  }

  set onopen(listener: (() => void) | null) {
    this.#source.onopen = listener;
  }

  addEventListener(
    type: 'change' | 'reset',
    listener: (event: EventMessage) => void,
  ): void {
    this.#source.addEventListener(type, (event) => {
      listener({ data: (event as MessageEvent<string>).data });
    });
  }

  close(): void {
    this.#source.close();
  }
}
