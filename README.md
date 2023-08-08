```
                 888      888      d8b          888      
                 888      888      Y8P          888      
                 888      888                   888      
888d888 888  888 88888b.  88888b.  888 .d8888b  88888b.  
888P"   888  888 888 "88b 888 "88b 888 88K      888 "88b 
888     888  888 888  888 888  888 888 "Y8888b. 888  888 
888     Y88b 888 888 d88P 888 d88P 888      X88 888  888 
888      "Y88888 88888P"  88888P"  888  88888P' 888  888 
```

![screenshot](https://github.com/superDuperCyberTechno/rubbish/raw/main/screenshot.jpg)

_rubbish_ is a simple thing: An application for viewing data dumps when you're programming. 

Why is that neat? - Well, at least for web development, it's not unusual to dump data in both the browser window, browser console and your local terminal client which means you're jumping back and forth like a flamin' bloody drunko trying to debug something stupid. It might also be a pretty mind numbing task to keep wrangling the data into something that is actually readable. With _rubbish_ you simply chug a string payload at it.

It's inspired by Spatie's [Ray](https://myray.app/) which is an awesome idea - however, I was ultimately frustrated by it which is why _rubbish_ exists.

## How?
Send a payload with POST to `http://localhost:7771`

I'm gonna go ahead and claim that this is compatible with all programming languages out there. At least the ones that are even remotely serious.

#### JSON
If your payload is valid JSON, it will be parsed. If "Interactive" is enabled, well it's interactive.

#### Title
You can add a title to the dump by adding the header `rubbish-title` (along with the actual title).

## Examples
If you have a function for _rubbish_ in your preferred language, please don't hesitate to request having it listed here.

#### PHP
```
function rub($dump, $title = null)
{
    $ch = curl_init("http://localhost:7771");
    curl_setopt($ch, CURLOPT_POST, 1);
    curl_setopt($ch, CURLOPT_POSTFIELDS, json_encode($dump));
    curl_setopt($ch, CURLOPT_ENCODING, "UTF-8");
    if ($title) {
        curl_setopt($ch, CURLOPT_HTTPHEADER, ["rubbish-title: {$title}"]);
    }
    curl_exec($ch);
}
```

# Downloads

|Linux|Windows|
|---|---|
|[.AppImage](https://github.com/superDuperCyberTechno/rubbish/raw/main/dist/rubbish.AppImage)|[.exe](https://github.com/superDuperCyberTechno/rubbish/raw/main/dist/rubbish.exe)|
