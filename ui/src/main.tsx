import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { ConfigureWindow } from './components/ConfigureWindow';
import { tauriRunLimitsApi } from './runLimits';
// Fonts and icon glyphs are bundled locally (local-first: no runtime fetch).
import '@fontsource/inter/400.css';
import '@fontsource/inter/500.css';
import '@fontsource/inter/600.css';
import '@fontsource/inter/700.css';
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@fontsource/jetbrains-mono/600.css';
import '@fontsource/jetbrains-mono/700.css';
import 'material-symbols/outlined.css';
import './styles.css';

// #1633: the menubar Configure window loads this bundle with ?view=configure.
const view = new URLSearchParams(window.location.search).get('view');

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    {view === 'configure' ? <ConfigureWindow api={tauriRunLimitsApi} /> : <App />}
  </React.StrictMode>,
);
