const { app, BrowserWindow, ipcMain } = require('electron')
const isDev = process.env.APP_DEV ? (process.env.APP_DEV.trim() == "true") : false;
const path = require('path')

let win
const createWindow = () => {
    win = new BrowserWindow({
        width: 1000,
        height: 600,
        // icon: __dirname + '/icon.png',
        webPreferences: {
            preload: path.join(__dirname, 'preload.js')
        }
    })

    win.setMenu(null)
    win.loadFile('index.html')
}

ipcMain.handle('getVersion', function(){ 
    return app.getVersion()
});

app.whenReady().then(() => {
    createWindow()
    if (isDev) {
        win.webContents.openDevTools();
    }

    const http = require('http')
    const server = http.createServer((req, res) => {
        if (req.method !== 'POST' || req.url !== '/') {
            res.statusCode = 405
            res.end()
            return
        }

        let rubbish_title = req.headers['rubbish-title']

        let body = ''
        req.on('data', (data) => {
            body += data
        })

        body = body.trim()

        req.on('end', () => {
            let dump
            let treat_as_string = false
            let is_valid_json = false

            try {
                dump = JSON.parse(body)
            } catch (e) {
                dump = body
            }

            if (validate_json(dump)) {
                is_valid_json = true
            } else {
                treat_as_string = true
            }

            dump = { 
                "title": rubbish_title,
                "dump": dump,
                "treat_as_string": treat_as_string,
                "is_valid_json": is_valid_json
            }

            win.webContents.send('dump', dump)
            res.end()
        });
    }).listen(7771)
})

function validate_json(json) {
    return (typeof json === 'object');
}

