// Thin wrapper over the Tauri bridge. Every backend call goes through here.

const T = window.__TAURI__;
const invoke = (cmd, args) => T.core.invoke(cmd, args);

// Custom protocols are exposed as http://<scheme>.localhost on Windows.
const PROTO = /Windows/.test(navigator.userAgent) ? "http://prism.localhost/" : "prism://localhost/";

const bust = new Map(); // asset id -> version (bumped when an edit changes the rendered look)

export const urls = {
  thumb: (a) => `${PROTO}thumb/${a.id}?m=${a.mtime}&v=${bust.get(a.id) || 0}`,
  view: (a, max = 2560) => `${PROTO}view/${a.id}?max=${max}&m=${a.mtime}&v=${bust.get(a.id) || 0}`,
  invalidate: (id) => bust.set(id, (bust.get(id) || 0) + 1),
};

export const api = {
  addFolder: (path, recursive = true) => invoke("add_folder", { path, recursive }),
  rescanFolder: (path) => invoke("rescan_folder", { path }),
  folders: () => invoke("folders"),
  search: (query) => invoke("search", { query }),
  details: (id) => invoke("asset_details", { id }),
  setRating: (ids, rating) => invoke("set_rating", { ids, rating }),
  setColorLabel: (ids, label) => invoke("set_color_label", { ids, label }),
  setTagged: (ids, tagged) => invoke("set_tagged", { ids, tagged }),
  setCaption: (id, caption) => invoke("set_caption", { id, caption }),
  addKeyword: (ids, keyword) => invoke("add_keyword", { ids, keyword }),
  removeKeyword: (ids, keyword) => invoke("remove_keyword", { ids, keyword }),
  keywords: () => invoke("keywords"),
  collections: () => invoke("collections"),
  createCollection: (name) => invoke("create_collection", { name }),
  addToCollection: (collectionId, ids) => invoke("add_to_collection", { collectionId, ids }),
  removeFromCollection: (collectionId, ids) => invoke("remove_from_collection", { collectionId, ids }),
  savedSearches: () => invoke("saved_searches"),
  saveSearch: (name, query) => invoke("save_search", { name, query }),
  deleteSavedSearch: (id) => invoke("delete_saved_search", { id }),
  similar: (id, maxDistance = 10) => invoke("similar", { id, maxDistance }),
  developGet: (id) => invoke("develop_get", { id }),
  developSave: (id, settings) => invoke("develop_save", { id, settings }),
  developAuto: (id) => invoke("develop_auto", { id }),
  developAutoWb: (id) => invoke("develop_auto_wb", { id }),
  developPreview: (id, settings, max) => invoke("develop_preview", { id, settings, max }),
  export: (ids, dir, quality, maxDim, caption = null) => invoke("export", { ids, dir, quality, maxDim, caption }),
  captionPreview: (id, caption) => invoke("caption_preview", { id, caption }),
  pickFolder: (title) => invoke("plugin:dialog|open", { options: { directory: true, multiple: false, title } }),
  listen: (event, cb) => T.event.listen(event, (e) => cb(e.payload)),
};
