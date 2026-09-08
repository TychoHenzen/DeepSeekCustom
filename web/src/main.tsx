import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './app/App.tsx';
import { AppErrorBoundary } from './app/AppErrorBoundary.tsx';
import './app/app.css';
import { BrowserEventClient } from './client/browser/BrowserEventClient.ts';

const root = document.querySelector<HTMLDivElement>('#root');
if (root === null) {
  throw new Error('DeepSeekCustom frontend root is missing');
}

createRoot(root).render(
  <StrictMode>
    <AppErrorBoundary>
      <App client={new BrowserEventClient()} />
    </AppErrorBoundary>
  </StrictMode>,
);
