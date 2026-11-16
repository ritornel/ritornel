#!/usr/bin/env bash
# Writes the audio fixtures in this folder again. They are committed, so
# nothing that runs the tests needs ffmpeg: this is here to say how they
# were made, and to make them again if one is ever added.
#
# Every file is synthetic: half a second of a sine tone, mono, 16 kHz, with
# invented tags. `a` and `b` are different tones. A name with `retagged` in
# it holds the audio of the file it is named after under other tags, by
# stream copy, so its audio hash must be the same. The hashes are pinned in
# crates/ritornel-engine/src/media_fixtures.rs.
#
# Run it with the ffmpeg that mise.toml pins: `mise exec -- fixtures/audio/generate.sh`.
set -euo pipefail
cd "$(dirname "$0")"

want="9.0.2"
found=$(ffmpeg -version | head -1 | awk '{ print $3 }')
if [ "$found" != "$want" ]; then
    echo "error: ffmpeg $found, and the fixtures are made with $want" >&2
    exit 1
fi
FF="ffmpeg -hide_banner -loglevel error -nostdin -y -bitexact"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
$FF -f lavfi -i "sine=f=440:r=16000:d=0.5" -ac 1 -c:a pcm_s16le "$tmp/a.wav"
$FF -f lavfi -i "sine=f=660:r=16000:d=0.5" -ac 1 -c:a pcm_s16le "$tmp/b.wav"
$FF -f lavfi -i "color=c=0x336699:s=8x8" -frames:v 1 "$tmp/cover.png"

# The tags a file tagged by Picard carries, with invented values.
PICARD=(
    -metadata "title=Paper Kites" -metadata "artist=Moss Arcade"
    -metadata "album=Tin Weather" -metadata "album_artist=Moss Arcade"
    -metadata "track=2/9" -metadata "disc=1/1" -metadata "date=2019-03-08"
    -metadata "ORIGINALDATE=2004-10-11" -metadata "RELEASETYPE=ep"
    -metadata "MUSICBRAINZ_TRACKID=11111111-2222-4333-8444-555555555555"
    -metadata "MUSICBRAINZ_ALBUMID=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
    -metadata "MUSICBRAINZ_RELEASEGROUPID=99999999-8888-4777-8666-555555555555"
    -metadata "MUSICBRAINZ_ARTISTID=12121212-3434-4565-8787-909090909090"
    -metadata "X_INVENTED=not understood, and kept"
)
PLAIN=(-metadata "title=Paper Kites" -metadata "artist=Moss Arcade" -metadata "album=Tin Weather" -metadata "track=2")
OTHER=(-metadata "title=Another Name Entirely" -metadata "artist=Somebody Else" -metadata "comment=retagged")

# FLAC, with a cover embedded.
$FF -i "$tmp/a.wav" -i "$tmp/cover.png" -map 0:a -map 1:v -c:a flac -c:v copy \
    -disposition:v attached_pic "${PICARD[@]}" a.flac
$FF -i a.flac -map 0:a -c copy -map_metadata -1 "${OTHER[@]}" a.retagged.flac
# An ID3v1 tag after the last frame, which the demuxer would read as audio.
{ cat a.flac; printf 'TAG%-125s' 'Paper Kites'; } > a.id3v1.flac
$FF -i "$tmp/b.wav" -c:a flac "${PLAIN[@]}" b.flac
# The same song as b.flac by its tags, on a compilation: another album.
$FF -i b.flac -map 0:a -c copy -map_metadata -1 -metadata "title=Paper Kites" \
    -metadata "artist=Moss Arcade" -metadata "album=Night Drives" \
    -metadata "album_artist=Various Artists" -metadata "compilation=1" -metadata "track=7" \
    b.compilation.flac

# MP3: ID3v2.4, with Picard's user-defined frames.
$FF -i "$tmp/a.wav" -c:a libmp3lame -b:a 32k -id3v2_version 4 "${PLAIN[@]}" \
    -metadata "album_artist=Moss Arcade" -metadata "TDOR=2004" \
    -metadata "MusicBrainz Album Type=album/live" \
    -metadata "MusicBrainz Album Id=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee" a.mp3
$FF -i a.mp3 -c copy -map_metadata -1 -id3v2_version 3 -write_id3v1 1 "${OTHER[@]}" a.retagged.mp3

# AAC and ALAC in MP4.
$FF -i "$tmp/a.wav" -c:a aac -b:a 32k "${PLAIN[@]}" -metadata "album_artist=Moss Arcade" \
    -metadata "disc=1/2" -metadata "compilation=1" a.m4a
$FF -i a.m4a -c copy -map_metadata -1 -movflags +faststart "${OTHER[@]}" a.retagged.m4a
$FF -i "$tmp/a.wav" -c:a alac "${PLAIN[@]}" a.alac.m4a
# Picard's identifiers in an M4A, as iTunes freeform atoms. ffmpeg cannot
# write those, so freeform.py adds them to a copy of a.m4a, audio untouched.
python3 freeform.py a.m4a a.musicbrainz.m4a \
    "MusicBrainz Track Id=11111111-2222-4333-8444-555555555555" \
    "MusicBrainz Release Track Id=33333333-4444-4555-8666-777777777777" \
    "MusicBrainz Album Id=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee" \
    "MusicBrainz Release Group Id=99999999-8888-4777-8666-555555555555" \
    "MusicBrainz Artist Id=12121212-3434-4565-8787-909090909090" \
    "MusicBrainz Album Artist Id=12121212-3434-4565-8787-909090909090" \
    "MusicBrainz Album Type=album" \
    "ARTISTS=Moss Arcade" \
    "ORIGINALDATE=2004-10-11" \
    "ISRC=XXA000000001" \
    "Acoustid Id=56565656-7878-4909-8121-343434343434"

# A file with no audio packets: an MP4 cut where its audio data begins.
$FF -i "$tmp/b.wav" -c:a aac -b:a 32k -movflags +faststart "${PLAIN[@]}" "$tmp/whole.m4a"
cut=$(LC_ALL=C grep --byte-offset --only-matching --text mdat "$tmp/whole.m4a" | head -1 | cut -d: -f1)
head -c "$((cut + 4))" "$tmp/whole.m4a" > no-packets.m4a

# Vorbis and Opus in Ogg.
$FF -i "$tmp/a.wav" -c:a libvorbis -q:a 0 "${PLAIN[@]}" a.ogg
$FF -i a.ogg -c copy -map_metadata -1 "${OTHER[@]}" a.retagged.ogg
$FF -i "$tmp/a.wav" -c:a libopus -b:a 16k "${PLAIN[@]}" a.opus
$FF -i a.opus -c copy -map_metadata -1 "${OTHER[@]}" a.retagged.opus

# PCM.
$FF -i "$tmp/a.wav" -c copy "${PLAIN[@]}" a.wav
$FF -i "$tmp/a.wav" -c:a pcm_s16be "${PLAIN[@]}" a.aiff

# Not audio at all, under an audio file's name.
printf 'this is not a FLAC file\n' > not-audio.flac

# What ffmpeg decodes two of them to, in ../decoded, which the player's
# decoding is held to: the FLAC's 16-bit samples exactly, and the MP3's float
# samples within what two decoders may differ by.
mkdir -p ../decoded
$FF -i a.flac -map 0:a -c:a pcm_s16le -f s16le ../decoded/a.flac.s16le
$FF -i a.mp3 -map 0:a -c:a pcm_f32le -f f32le ../decoded/a.mp3.f32le

ls -l | awk 'NR > 1 { total += $5 } END { print NR - 1 " files, " total " bytes" }'
