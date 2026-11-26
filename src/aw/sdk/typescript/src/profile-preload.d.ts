import type { ProfileSource } from "./profile-source.js";

/** The Source configured by AW_PROFILE_BOOTSTRAP, if set before Node startup. */
export const preloadedProfileSource: ProfileSource | undefined;
