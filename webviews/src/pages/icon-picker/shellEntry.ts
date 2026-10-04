// The icon picker as a page-shell page (src/pages/shell): mounted on `page.claim` with the claim's
// context as its first session, so the popover shows a ready picker with no load. Styles go
// through the shell, which removes them on reset.
import type { ShellContext } from "../shell/shell";
import pageStyles from "../../icon-picker/styles.css?inline";
import shellStyles from "./styles.css?inline";
import table from "./generated/strings.json";
import type { PickerSession } from "./host";
import { mountIconPicker } from "./mount";

export function mount(root: HTMLElement, ctx: ShellContext): { unmount(): void } {
  ctx.style(pageStyles);
  ctx.style(shellStyles);
  const picker = mountIconPicker(root, ctx.client, ctx.strings(table));
  const session = ctx.context as PickerSession | null | undefined;
  if (session && typeof session.id === "string" && session.id) picker.open(session);
  return { unmount: () => picker.unmount() };
}
