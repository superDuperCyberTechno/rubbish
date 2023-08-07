const { app, contextBridge, ipcRenderer } = require("electron")

contextBridge.exposeInMainWorld("api", {
    "dump": function(callback) {
        ipcRenderer.on("dump", callback)
    },
    getVersion: () => ipcRenderer.invoke('getVersion')
})
