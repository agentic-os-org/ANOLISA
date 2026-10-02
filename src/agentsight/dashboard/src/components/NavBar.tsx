import React from 'react';
import { NavLink, useLocation } from 'react-router-dom';
import { LanguageSwitcher, useI18n } from '../i18n';
import { NAV_ROUTES } from '../utils/navigation';
import type { AppCapability } from '../utils/apiClient';

interface NavBarProps {
  capabilities?: AppCapability[];
}

export const NavBar: React.FC<NavBarProps> = ({ capabilities }) => {
  const location = useLocation();
  const { t } = useI18n();
  const visibleItems = Array.isArray(capabilities)
    ? NAV_ROUTES.filter((item) => capabilities.includes(item.capability))
    : NAV_ROUTES;

  return (
    <nav className="bg-white border-b border-gray-200 px-6 py-3">
      <div className="max-w-screen-2xl mx-auto flex flex-wrap items-center gap-3">
        {/* Logo / Brand */}
        <div className="flex items-center gap-2">
          <span className="text-xl font-bold text-gray-900">AgentSight</span>
          <span className="text-xs text-gray-400 px-2 py-0.5 bg-gray-100 rounded">v1.0</span>
        </div>

        {/* Navigation Links */}
        <div className="flex flex-1 flex-wrap items-center justify-end gap-1">
          {visibleItems.map((item) => {
            // Match on a `/` boundary so a route never claims a sibling path
            // that shares its prefix, and so no entry needs the exact-root
            // special case the bare `/` observability link used to require.
            const isActive = location.pathname === item.path
              || location.pathname.startsWith(`${item.path}/`);

            return (
              <NavLink
                key={item.path}
                to={item.path}
                className={`px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
                  isActive
                    ? 'bg-blue-100 text-blue-700'
                    : 'text-gray-600 hover:bg-gray-100 hover:text-gray-900'
                }`}
              >
                <span className="mr-1.5">{item.icon}</span>
                {t(item.labelKey)}
              </NavLink>
            );
          })}
          <LanguageSwitcher id="navbar-language" className="ml-2 shrink-0" />
        </div>
      </div>
    </nav>
  );
};
