import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App, browserClient } from './app/App.tsx';
import './app/app.css';
import { ApplicationClient } from './client/client.ts';

const root = document.querySelector<HTMLDivElement>('#root');
if (root === null) {
  throw new Error('DeepSeekCustom frontend root is missing');
}

createRoot(root).render(
  <StrictMode>
    <App client={browserClient(new ApplicationClient())} />
  </StrictMode>,
);
