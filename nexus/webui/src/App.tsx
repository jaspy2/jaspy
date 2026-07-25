import { useState } from 'react';
import { NavLink, Route, Routes, useMatch } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import { api } from './api/client';
import Dashboard from './pages/Dashboard';
import Devices from './pages/Devices';
import DeviceDetail from './pages/DeviceDetail';
import Discovery from './pages/Discovery';
import ClientLocations from './pages/ClientLocations';
import Maintenance from './pages/Maintenance';
import Vlans from './pages/Vlans';
import STP from './pages/STP';
import Issues from './pages/Issues';

const NAV = [
  { to: '/', label: 'Dashboard' },
  { to: '/issues', label: 'Issues' },
  { to: '/devices', label: 'Devices' },
  { to: '/vlans', label: 'VLANs' },
  { to: '/stp', label: 'STP' },
  { to: '/discovery', label: 'Discovery' },
  { to: '/clients', label: 'Client locations' },
  { to: '/maintenance', label: 'Maintenance' },
];

export default function App() {
  const [navOpen, setNavOpen] = useState(false);
  const summary = useQuery({ queryKey: ['summary'], queryFn: api.summary, refetchInterval: 10000 });
  const system = useQuery({ queryKey: ['system'], queryFn: api.system });
  const megaexcelUrl = system.data?.megaexcelUrl ?? null;
  const deviceMatch = useMatch('/devices/:fqdn');
  const deviceFqdn = deviceMatch?.params.fqdn;

  return (
    <div className="app">
      <header className="topbar">
        <button className="hamburger" onClick={() => setNavOpen(!navOpen)} aria-label="Toggle navigation">
          ☰
        </button>
        <span className="brand" title={deviceFqdn ?? 'jaspy'}>
          {deviceFqdn ? (
            <>
              <span className="brand-prefix hide-mobile">jaspy — </span>
              {deviceFqdn}
            </>
          ) : (
            'jaspy'
          )}
        </span>
        <span className="event-name">{summary.data?.eventName ?? 'no event'}</span>
        <span className="spacer" />
        {summary.data && (
          <span className="header-badges">
            <span className="badge badge-ok" title="devices up">{summary.data.devicesUp} up</span>
            <span className="badge badge-bad" title="devices down">{summary.data.devicesDown} down</span>
            {summary.data.devicesUnknown > 0 && (
              <span className="badge badge-muted" title="devices with unknown state">{summary.data.devicesUnknown} unknown</span>
            )}
          </span>
        )}
      </header>
      <div className="body">
        {navOpen && <div className="backdrop" onClick={() => setNavOpen(false)} />}
        <nav className={`sidebar ${navOpen ? 'open' : ''}`}>
          {NAV.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={item.to === '/'}
              className={({ isActive }) => (isActive ? 'nav-link active' : 'nav-link')}
              onClick={() => setNavOpen(false)}
            >
              {item.label}
            </NavLink>
          ))}
          <a className="nav-link" href="https://mobydick.netcrew.fi/wmap/" target="_blank" rel="noreferrer">
            Weathermap ↗
          </a>
          {megaexcelUrl && (
            <a className="nav-link" href={`${megaexcelUrl}/auth?I=kissa123`} target="_blank" rel="noreferrer">
              Megaexcel ↗
            </a>
          )}
        </nav>
        <main className="content">
          <Routes>
            <Route path="/" element={<Dashboard />} />
            <Route path="/issues" element={<Issues />} />
            <Route path="/devices" element={<Devices />} />
            <Route path="/devices/:fqdn" element={<DeviceDetail />} />
            <Route path="/vlans" element={<Vlans />} />
            <Route path="/stp" element={<STP />} />
            <Route path="/discovery" element={<Discovery />} />
            <Route path="/clients" element={<ClientLocations />} />
            <Route path="/maintenance" element={<Maintenance />} />
          </Routes>
        </main>
      </div>
    </div>
  );
}
