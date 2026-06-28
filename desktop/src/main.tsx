import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './fonts.css';
import './styles.css';

// Dev-only automation bridge (tree-shaken out of production builds).
if (import.meta.env.DEV) {
  void import('./lib/devbridge').then((m) => m.startDevBridge()).catch(() => {});
}

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
