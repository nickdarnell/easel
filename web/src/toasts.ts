import { reactive } from "vue";

export interface Toast {
  id: number;
  text: string;
  err: boolean;
}

export const toasts = reactive<Toast[]>([]);
let nextId = 1;

export interface ToastHandle {
  remove(): void;
  set(text: string): void;
}

export function toast(text: string, err = false, ms = 4000): ToastHandle {
  const id = nextId++;
  toasts.push({ id, text, err });
  const remove = () => {
    const i = toasts.findIndex((t) => t.id === id);
    if (i >= 0) toasts.splice(i, 1);
  };
  setTimeout(remove, err ? Math.max(ms, 7000) : ms);
  return {
    remove,
    set(text) {
      const t = toasts.find((t) => t.id === id);
      if (t) t.text = text;
    },
  };
}
