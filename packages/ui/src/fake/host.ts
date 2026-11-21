// The desktop's part, faked for the browser: fixed picks, notifications recorded.
import type { Host } from "../engine/client";

export const notices: { title: string; body: string }[] = [];

export const fakeHost: Host = {
  pickFolder: async () => "/Users/person/work/estimating-demo",
  pickFiles: async () => ["/Users/person/Desktop/site-photo.jpg", "/Users/person/Downloads/tender-addendum.pdf"],
  openUrl: async () => {},
  async notify(title, body) {
    notices.push({ title, body });
    if (typeof Notification !== "undefined" && Notification.permission === "granted") new Notification(title, { body });
  },
};
