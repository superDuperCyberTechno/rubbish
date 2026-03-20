# rubbish
Terminal-based dump-viewer for local software development. Fire up rubbish and throw JSON at `http://127.0.0.1:7771`. Then use rubbish to browse the data at your own leisure.

## Features
- Interactive TUI.
- Navigate with arrow keys or Vim-keys.
- Use space to filter dumps by tags (only usable in the tags-box).
- Press `c` to clear the tags filter.
- Pressing `Enter` when focusing a dump will open an appropriate pager (only usable in the dumps-box). `jless` is the default pager, otherwise `less`. If neither are available, nothing will happen.
- Auto-focuses the newest dump automatically (if it passes the current tag filter).

## How?
Implement a simple helper function in any of your development projects that dumps valid JSON to 127.0.0.1:7771. The built-in webserver in rubbish will receive, validate, format and save the data for perusing.

All dumps are saved in `~/.local/share/rubbish/dumps`.

### Additional data
rubbish supports some unique headers to make your life a little easier...
- `rubbish-title`: Provide a title for your dump. This title will be displayed at the top of the dump preview.
- `rubbish-tags`: A comma-separated string of tags for your dump. All tags received will be sorted and presented in the Tags-box. Tags can then be toggled for filtering out dumps. Press `c` to clear the tag filter.

### Examples

#### PHP
```php
function rub($dump, $title = null, $tags = [])
{
    $ch = curl_init('http://127.0.0.1:7771/');
    curl_setopt($ch, CURLOPT_POST, 1);
    curl_setopt($ch, CURLOPT_POSTFIELDS, json_encode($dump) . '');
    curl_setopt($ch, CURLOPT_ENCODING, 'UTF-8');

    $header = [];
    if ($title) {
        $header[] = "rubbish-title: {$title}";
    }

    if ($tags) {
        $tags = implode(',', $tags);
        $header[] = "rubbish-tags: {$tags}";
    }

    curl_setopt($ch, CURLOPT_HTTPHEADER, $header);
    curl_exec($ch);
}
```
