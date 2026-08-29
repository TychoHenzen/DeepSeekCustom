import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

const root = document.querySelector<HTMLDivElement>('#root');
if (root === null) {
  throw new Error('DeepSeekCustom frontend root is missing');
}

createRoot(root).render(
  <StrictMode>
    <main>
      <h1>DeepSeekCustom</h1>
      <p>The web frontend workspace is ready.</p>
    </main>
  </StrictMode>,
);
