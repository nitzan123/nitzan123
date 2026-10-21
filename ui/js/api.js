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
  face: (faceId) => `${PROTO}face/${faceId}`,
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
  /** options: { format, quality, maxDim, caption, nameTemplate } */
  export: (ids, dir, options) => invoke("export", { ids, dir, options }),
  presets: () => invoke("presets"),
  savePreset: (name, settings) => invoke("save_preset", { name, settings }),
  deletePreset: (id) => invoke("delete_preset", { id }),
  applyPreset: (ids, settings) => invoke("apply_preset_to", { ids, settings }),
  renamePreview: (ids, template, start) => invoke("rename_preview", { ids, template, start }),
  renameApply: (ids, template, start) => invoke("rename_apply", { ids, template, start }),
  captionPreview: (id, caption) => invoke("caption_preview", { id, caption }),
  lensDbStatus: () => invoke("lens_db_status"),
  lensDbDownload: () => invoke("lens_db_download"),
  lensProfile: (id) => invoke("lens_profile", { id }),
  aiStatus: () => invoke("ai_status"),
  aiDownload: () => invoke("ai_download"),
  aiAnalyze: () => invoke("ai_analyze"),
  aiCancel: () => invoke("ai_cancel"),
  aiDetails: (id) => invoke("ai_details", { id }),
  smartSearch: (text, limit) => invoke("smart_search", { text, limit }),
  persons: () => invoke("persons"),
  namePerson: (personId, name) => invoke("name_person", { personId, name }),
  setFaceName: (faceId, name) => invoke("set_face_name", { faceId, name }),
  aiMask: (id, kind) => invoke("ai_mask", { id, kind }),
  pickFolder: (title) => invoke("plugin:dialog|open", { options: { directory: true, multiple: false, title } }),
  listen: (event, cb) => T.event.listen(event, (e) => cb(e.payload)),
};
