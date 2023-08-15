let dumps_container = document.getElementById("dumps")

window.api.getVersion().then(function(version){
    document.getElementById('version').textContent = version
})

let autojump_enabled = true,
    interactive_enabled = false,
    last_dump_timestamp = '',
    delta = null

window.api.dump((event, dump) => {
    let now = new Date();

    if (last_dump_timestamp) {
        delta = ms_diff(now, last_dump_timestamp)
    }

    let offset = now.getTimezoneOffset()
    now_actual = new Date(now.getTime() - (offset * 60 * 1000))
    let timestamp = now_actual.toISOString().split('T')[1]
    timestamp = timestamp.substring(0, timestamp.length - 1)

    let dump_string
    if (dump.treat_as_string || !dump.is_valid_json) {
        dump_string = dump.dump
    } else {
        dump_string = JSON.stringify(dump.dump, undefined, 2)
    }

    let lines =  dump_string.split(/\r\n|\r|\n/).length

    let header = document.createElement('div')
    header.classList.add('header')
    header.style.background = rand_color()
    header.onclick = function() {
        if (this.nextSibling.classList.contains('closed')) {
            this.nextSibling.classList.remove('closed')
        } else {
            this.nextSibling.classList.add('closed')
        }
    }

    let header_content = ''
    header_content += timestamp 
    if (delta) {
        header_content += '|Δ'+delta
    }

    header_content += '<div class="dump-info">'+lines + (lines > 1 ? ' lines' : ' line') + '<br>'+((dump.is_valid_json) ? 'JSON' : 'plaintext')+'</div>'
    header_content += dump.title ?? ''

    header.insertAdjacentHTML('beforeend', header_content)
    dumps_container.appendChild(header)

    if (interactive_enabled && !dump.treat_as_string) {
        dump_element = '<div class="dump" id="'+timestamp+'_data"></div>'
    } else {
        dump_element = '<pre class="dump" id="'+timestamp+'_data">' + dump_string + '</pre>'
    }

    let details = dump_element + '<div class="copy-blocker"></div>'
    dumps_container.insertAdjacentHTML("beforeend", details)

    if (interactive_enabled && ! dump.treat_as_string) {
        jsonTree.create(dump.dump, document.getElementById(timestamp + '_data'));
    }

    modify_spacer_height()

    if (autojump_enabled) {
        let last_header = dumps_container.lastChild.previousSibling.previousSibling
        last_header.scrollIntoView()
    }

    last_dump_timestamp = now
})

function modify_spacer_height() {
    let dump_height = document.getElementById('dumps').lastChild.previousSibling.offsetHeight
    let spacer_height = '50vh'
    if (dump_height < window.innerHeight) {
        spacer_height = (window.innerHeight - dump_height) + 'px'
    }
    document.getElementById('spacer').setAttribute("style","height:" + spacer_height + ';');
}

function toggle_autojump() {
    if (autojump_enabled) {
        autojump_enabled = false
        document.getElementById('autojump_status').textContent = 'Disabled'
    } else {
        autojump_enabled = true
        document.getElementById('autojump_status').textContent = 'Enabled'
    }
}

function toggle_interactive() {
    if (interactive_enabled) {
        interactive_enabled = false
        document.getElementById('interactive_status').textContent = 'Disabled'
    } else {
        interactive_enabled = true
        document.getElementById('interactive_status').textContent = 'Enabled'
    }
}

document.getElementById('interactive').addEventListener('click', function() { 
    toggle_interactive()
}, false)

document.getElementById('autojump').addEventListener('click', function() { 
    toggle_autojump()
}, false)

document.getElementById('fold').addEventListener('click', function() { 
    fold()
}, false)

document.getElementById('empty').addEventListener('click', function() { 
    empty()
}, false)

document.getElementById('last').addEventListener('click', function() { 
    goto_last()
}, false)

document.addEventListener("copy", async (e) => {
    e.preventDefault()
    let selected = window.getSelection().toString().trim()
    await navigator.clipboard.writeText(selected)
});

window.addEventListener('keyup', function(event) {
    if (event.keyCode === 9 || event.keyCode === 32) {
        event.preventDefault()
    }
})

window.addEventListener('keydown', function(event) {
    if (document.activeElement.tagName === "INPUT") {
        return
    }

    else if (event.keyCode === 9 || event.keyCode === 32) {
        event.preventDefault()
    }

    else if (event.key === 'e') {
        empty()
    }

    else if (event.key === 'f') {
        fold()
    }

    else if (event.key === 'a') {
        toggle_autojump()
    }

    else if (event.key === 'i') {
        toggle_interactive()
    }

    else if (event.key === 'l') {
        goto_last()
    }
}, true)

function toggle_dump() {
    console.log(this)
    console.log(this.nextSibling)
}

function fold(){
    let elems = document.querySelectorAll('.dump')
    for (let i=0; i<elems.length; i++) {
        elems[i].classList.add('closed')
    }
}

function goto_last() {
    let last_dump = dumps_container.lastChild.previousSibling
    last_dump.classList.remove('closed')
    let last_header = dumps_container.lastChild.previousSibling.previousSibling
    last_header.scrollIntoView()
}

function empty() {
    document.getElementById("dumps").textContent = ''
}

let hsl_color_angle = Math.random() * 360
function rand_color() {
    hsl_color_angle = (hsl_color_angle + 140 + Math.random() * 40) % 360
    color = "hsl(" + hsl_color_angle + ", 70%, 65%)"
    return color
}

function ms_diff(date1, date2) {
    let diff = date1.getTime() - date2.getTime()

    if (diff > 60000) {
        diff = Math.round(diff / 60000) + 'm'
    }

    else if (diff > 1000) {
        diff = Math.round(diff / 1000) + 's'
    }

    else {
        diff = diff + 'ms'
    }

    return diff
}
