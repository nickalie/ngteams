# The copy of a card and the ngteamsctl text of a card are different

The chat view copies a card's text with `teams_proto::chat::card::plain_words`. `ngteamsctl` and the search index
read it with `card::text` (`Lines`). The two walk the same card model with different rules: facts (`title value`
against `title: value`), markdown, hidden columns, actions and tables.

Do: make `plain_words` use `Lines`, with the date and time text as a parameter (the view shows local time, the ctl
UTC). Then remove `collect_words`, and update the server e2e tests that call `plain_words`.
