import Link from '@docusaurus/Link';
import type {Locale} from '../../content.config';

export function localizedPath(_locale: Locale, path: string) {
  return path;
}

export default function SiteLink({
  locale,
  to,
  children,
  className,
}: {
  locale: Locale;
  to: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <Link className={className} to={localizedPath(locale, to)}>
      {children}
    </Link>
  );
}
