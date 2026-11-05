import React, {useEffect} from 'react';
import Head from '@docusaurus/Head';

export default function DocumentRedirect({destination}: {destination: {to: string}}) {
  useEffect(() => {
    const target = new URL(destination.to, window.location.href);
    if (window.location.hash) target.hash = window.location.hash;
    window.location.replace(target.href);
  }, [destination.to]);
  return <>
    <Head><title>Documentation moved</title><meta name="robots" content="noindex" /></Head>
    <a href={destination.to}>Continue to documentation</a>
  </>;
}
