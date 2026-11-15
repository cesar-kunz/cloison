// expect: 4|a.b@c.d,x@y.z|10.0.0.1,192.168.1.20|http://a.b/c|12,7,0,123456789012345,NaN,1000,-0,12|3,0|ab
// Searches that skip places (a leading run, the first units, lookarounds
// of one unit) find what a search everywhere does; digit strings convert
// as any number does.
var text = 'aaaa.bbbb cccc a.b@c.d (x@y.z) 1234.5.6 10.0.0.1 999.1.1.1x 192.168.1.20 see http://a.b/c ftp:/x';
var words = 0, m, w = /[a-z]+(?:\.[a-z]+)*@/g;
var mails = text.match(/(?:[a-z0-9]+(?:\.[a-z0-9]+)*|"[^"]*")@[a-z]+\.[a-z]+/gi);
var b = '(?:25[0-5]|2[0-4]\\d|1?[0-9]\\d|\\d)';
var ips = text.match(new RegExp('(?:(?<!\\d)' + b + '\\.){3}(?:' + b + '(?!\\d))', 'ig'));
var urls = text.match(/[A-Z]+:\/\/[-\w]+(?:\.\w[-\w]*)+(?:\/\S*)?/ig);
while ((m = w.exec(text))) words++;
var nums = ['12', '007', '0', '123456789012345', '12a', '1e3', '-0', ' 12 '].map(function (s) { return Object.is(+s, -0) ? '-0' : String(s * 1); });
[words + mails.length, mails, ips, urls, nums, [/a+b/.exec('aaaaab').index + 3, 'xaaa'.search(/a*$/) - 1], /(?<=x)[ab]+(?=y)/.exec('ab xaby')[0]].join('|');
