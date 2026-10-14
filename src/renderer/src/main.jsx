import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import HistoryWindow from './components/HistoryWindow';
import './index.css';

// The same page opens as the history window (#history) in the Rust app.
const Page = window.location.hash === '#history' ? HistoryWindow : App;
createRoot(document.getElementById('root')).render(<Page />);
