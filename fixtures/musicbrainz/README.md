# MusicBrainz replies

Replies in the shape MusicBrainz's web service gives to a disc ID lookup (`/ws/2/discid/<id>?inc=artist-credits+release-groups+recordings&cdstubs=no&fmt=json`), for the tests. Every artist, release, ID and barcode in them is invented. The two discs are those of the engine's tests: three tracks of 75, 120 and 90 sectors, and a bonus disc of two tracks.

- `disc-tin-weather.json`: the first disc, on two releases of one album.
- `disc-tin-weather-bonus.json`: the bonus disc, on the second release only.
- `not-found.json`: what a disc MusicBrainz does not know gives, with status 404.
