| record | run | certified | typecheck | effects | refinement | termination | complexity | properties (generative) | prove |
|---|---|---|---|---|---|---|---|---|---|
| `div_floor` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | exact=EXHAUSTIVE; monotone=EXHAUSTIVE | exact=PROVED; monotone=PROVED |
| `div_ceil` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | exact=EXHAUSTIVE; dual_of_floor=EXHAUSTIVE | exact=PROVED; dual_of_floor=PROVED |
| `div_trunc` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND / post[2]=SOUND | SOUND | SOUND | odd_symmetric=EXHAUSTIVE | odd_symmetric=PROVED |
| `div_half_up` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | exact=EXHAUSTIVE; odd_symmetric=EXHAUSTIVE | exact=PROVED; odd_symmetric=PROVED |
| `div_half_even` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | exact=EXHAUSTIVE; odd_symmetric=EXHAUSTIVE; monotone=EXHAUSTIVE | exact=PROVED; odd_symmetric=PROVED; monotone=PROVED |
| `pow10` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | zero=EXHAUSTIVE; product=EXHAUSTIVE; increasing=EXHAUSTIVE | zero=PROVED; product=PROVED; increasing=PROVED |
| `rescale` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND / post[2]=SOUND | SOUND | SOUND | roundtrip=EXHAUSTIVE | roundtrip=PROVED |
| `apply_rate` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | unit_rate=EXHAUSTIVE; zero_amount=EXHAUSTIVE | unit_rate=PROVED; zero_amount=PROVED |
| `percent_bps` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | whole=EXHAUSTIVE | whole=PROVED |
| `fx_convert` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | — | — |
| `list_sum` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | empty=EXHAUSTIVE; cons=HELD; append=HELD | empty=PROVED; cons=PROVED; append=PROVED |
| `alloc_from` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | length=HELD; telescopes=HELD; quota=HELD | length=PROVED; telescopes=PROVED; quota=PROVED |
| `allocate` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | — | — |
| `format_amount` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `parse_amount` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `amount_text_roundtrip` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | UNVERIFIABLE | format_then_parse=EXHAUSTIVE | format_then_parse=UNKNOWN |
| `parse_decimal` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `round_text_half_even` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND / post[2]=SOUND | SOUND | UNVERIFIABLE | — | — |
| `fx_convert_text` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND / post[2]=SOUND | SOUND | UNVERIFIABLE | — | — |
