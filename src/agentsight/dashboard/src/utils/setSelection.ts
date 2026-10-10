/**
 * Whether two id sets hold exactly the same members.
 *
 * Selection shortcuts ("select all", "select pending", …) toggle: choosing a
 * criterion whose rows are already selected clears the selection. Comparing
 * sizes instead of members gets that wrong whenever the reviewer hand-picked
 * the same number of *different* rows — the shortcut then cleared a selection
 * the criterion never matched.
 */
export function sameMembers(current: Set<string>, next: Set<string>): boolean {
  if (current.size !== next.size) return false;
  for (const id of next) {
    if (!current.has(id)) return false;
  }
  return true;
}
