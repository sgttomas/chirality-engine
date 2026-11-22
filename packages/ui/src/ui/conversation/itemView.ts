// An application's own view of items: a node for an item, or `null` (or `undefined`) for
// the standard view.
import { createContext, type ReactNode } from "react";

import type { Item } from "../../engine/types";

export type ItemViewer = (item: Item) => ReactNode | null | undefined;

/** The application's view of items, read by `TurnView`; none by default. */
export const ItemView = createContext<ItemViewer | undefined>(undefined);
