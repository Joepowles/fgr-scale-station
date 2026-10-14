import React, { useEffect, useState } from 'react';
import HistoryDrawer from './HistoryDrawer';

// The history in a window of its own (issue #12): the drawer's list and
// photos, filling the window, kept up to date as trucks are weighed.
export default function HistoryWindow() {
  const [info, setInfo] = useState(null);
  useEffect(() => { window.station.info().then(setInfo); }, []);
  if (!info) return null;
  return <HistoryDrawer info={info} standalone />;
}
